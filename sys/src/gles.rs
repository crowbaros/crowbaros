//! The GLES renderer's GPU half: an EGL display and context, textures from a
//! client's shared memory or dma-bufs, targets to draw into, and the draws.
//!
//! EGL and GLES are opened at run time (see `dl.rs`). There is one context,
//! made current on the thread that made it and never on another: [`Gles`] is
//! neither `Send` nor `Sync`, and every texture and target holds the context
//! it belongs to. The compositor decides what to draw where; this module only
//! draws it.
//!
//! Coordinates are pixels with the origin at the top left. A target's row 0
//! is the top row, as the display scans it out, so nothing is flipped: GL's
//! y = 0 is row 0 of the target's memory.

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    io,
    os::fd::{AsRawFd, OwnedFd},
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::OnceLock,
};

use crate::{
    dl::{Library, functions},
    gbm,
};

type Display = *mut c_void;
type Context = *mut c_void;
type Image = *mut c_void;

const EGL_NONE: i32 = 0x3038;
const EGL_TRUE: u32 = 1;
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_PLATFORM_GBM_KHR: u32 = 0x31D7;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
const EGL_DEVICE_EXT: i32 = 0x322C;
const EGL_DRM_DEVICE_FILE_EXT: i32 = 0x3233;
const EGL_DRM_RENDER_NODE_FILE_EXT: i32 = 0x3377;
/// For each of four planes: its descriptor, offset, pitch, and modifier's
/// low and high halves.
const PLANE_ATTRIBUTES: [[i32; 5]; 4] = [
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327A, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344A],
];

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_RENDERBUFFER: u32 = 0x8D41;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_BLEND: u32 = 0x0BE2;
const GL_ONE: u32 = 1;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_FLOAT: u32 = 0x1406;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_RENDERER: u32 = 0x1F01;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;

functions! {
    /// The core of EGL, from libEGL.
    EglFunctions {
        fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
        fn eglGetError() -> i32;
        fn eglQueryString(display: Display, name: i32) -> *const c_char;
        fn eglInitialize(display: Display, major: *mut i32, minor: *mut i32) -> u32;
        fn eglTerminate(display: Display) -> u32;
        fn eglBindAPI(api: u32) -> u32;
        fn eglCreateContext(display: Display, config: *mut c_void, share: Context, attributes: *const i32) -> Context;
        fn eglDestroyContext(display: Display, context: Context) -> u32;
        fn eglMakeCurrent(display: Display, draw: *mut c_void, read: *mut c_void, context: Context) -> u32;
        fn eglGetCurrentContext() -> Context;
    }
}

functions! {
    /// EGL's extensions, from `eglGetProcAddress`.
    EglExtensions {
        fn eglGetPlatformDisplayEXT(platform: u32, native: *mut c_void, attributes: *const i32) -> Display;
        fn eglCreateImageKHR(display: Display, context: Context, target: u32, buffer: *mut c_void, attributes: *const i32) -> Image;
        fn eglDestroyImageKHR(display: Display, image: Image) -> u32;
    }
}

functions! {
    /// Listing the dma-buf formats and modifiers EGL imports.
    EglDmabufQueries {
        fn eglQueryDmaBufFormatsEXT(display: Display, max: i32, formats: *mut i32, count: *mut i32) -> u32;
        fn eglQueryDmaBufModifiersEXT(
            display: Display,
            format: i32,
            max: i32,
            modifiers: *mut u64,
            external_only: *mut u32,
            count: *mut i32,
        ) -> u32;
    }
}

functions! {
    /// Which DRM device an EGL display is on.
    EglDeviceQueries {
        fn eglQueryDisplayAttribEXT(display: Display, attribute: i32, value: *mut isize) -> u32;
        fn eglQueryDeviceStringEXT(device: *mut c_void, name: i32) -> *const c_char;
    }
}

functions! {
    /// GLES 2.0, from libGLESv2.
    GlFunctions {
        fn glGetString(name: u32) -> *const u8;
        fn glViewport(x: i32, y: i32, width: i32, height: i32);
        fn glScissor(x: i32, y: i32, width: i32, height: i32);
        fn glEnable(capability: u32);
        fn glDisable(capability: u32);
        fn glBlendFunc(source: u32, destination: u32);
        fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32);
        fn glClear(mask: u32);
        fn glPixelStorei(name: u32, value: i32);
        fn glGenTextures(count: i32, textures: *mut u32);
        fn glDeleteTextures(count: i32, textures: *const u32);
        fn glBindTexture(target: u32, texture: u32);
        fn glActiveTexture(unit: u32);
        fn glTexParameteri(target: u32, name: u32, value: i32);
        fn glTexImage2D(
            target: u32,
            level: i32,
            internal: i32,
            width: i32,
            height: i32,
            border: i32,
            format: u32,
            kind: u32,
            pixels: *const c_void,
        );
        fn glTexSubImage2D(
            target: u32,
            level: i32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            format: u32,
            kind: u32,
            pixels: *const c_void,
        );
        fn glGenFramebuffers(count: i32, framebuffers: *mut u32);
        fn glDeleteFramebuffers(count: i32, framebuffers: *const u32);
        fn glBindFramebuffer(target: u32, framebuffer: u32);
        fn glFramebufferTexture2D(target: u32, attachment: u32, texture_target: u32, texture: u32, level: i32);
        fn glFramebufferRenderbuffer(target: u32, attachment: u32, renderbuffer_target: u32, renderbuffer: u32);
        fn glCheckFramebufferStatus(target: u32) -> u32;
        fn glGenRenderbuffers(count: i32, renderbuffers: *mut u32);
        fn glDeleteRenderbuffers(count: i32, renderbuffers: *const u32);
        fn glBindRenderbuffer(target: u32, renderbuffer: u32);
        fn glCreateShader(kind: u32) -> u32;
        fn glShaderSource(shader: u32, count: i32, sources: *const *const c_char, lengths: *const i32);
        fn glCompileShader(shader: u32);
        fn glGetShaderiv(shader: u32, name: u32, value: *mut i32);
        fn glGetShaderInfoLog(shader: u32, max: i32, length: *mut i32, log: *mut c_char);
        fn glDeleteShader(shader: u32);
        fn glCreateProgram() -> u32;
        fn glAttachShader(program: u32, shader: u32);
        fn glBindAttribLocation(program: u32, index: u32, name: *const c_char);
        fn glLinkProgram(program: u32);
        fn glGetProgramiv(program: u32, name: u32, value: *mut i32);
        fn glGetProgramInfoLog(program: u32, max: i32, length: *mut i32, log: *mut c_char);
        fn glDeleteProgram(program: u32);
        fn glUseProgram(program: u32);
        fn glGetUniformLocation(program: u32, name: *const c_char) -> i32;
        fn glUniform1i(location: i32, value: i32);
        fn glUniform1f(location: i32, value: f32);
        fn glUniform2f(location: i32, x: f32, y: f32);
        fn glUniform4f(location: i32, x: f32, y: f32, z: f32, w: f32);
        fn glVertexAttribPointer(index: u32, size: i32, kind: u32, normalized: u8, stride: i32, pointer: *const c_void);
        fn glEnableVertexAttribArray(index: u32);
        fn glDrawArrays(mode: u32, first: i32, count: i32);
        fn glReadPixels(x: i32, y: i32, width: i32, height: i32, format: u32, kind: u32, pixels: *mut c_void);
        fn glFinish();
    }
}

functions! {
    /// Textures and renderbuffers from EGL images.
    GlImageFunctions {
        fn glEGLImageTargetTexture2DOES(target: u32, image: Image);
        fn glEGLImageTargetRenderbufferStorageOES(target: u32, image: Image);
    }
}

struct Libraries {
    egl: EglFunctions,
    gl: GlFunctions,
}

fn libraries() -> Option<&'static Libraries> {
    static LIBRARIES: OnceLock<Option<Libraries>> = OnceLock::new();
    LIBRARIES
        .get_or_init(|| {
            let egl = Library::open(&[c"libEGL.so.1", c"libEGL.so"])?;
            let gles = Library::open(&[c"libGLESv2.so.2", c"libGLESv2.so"])?;
            // A library is never closed, so the functions stay valid.
            Some(Libraries {
                egl: EglFunctions::load(|name| egl.symbol(name)).ok()?,
                gl: GlFunctions::load(|name| gles.symbol(name)).ok()?,
            })
        })
        .as_ref()
}

/// Whether EGL and GLES could be loaded.
pub fn available() -> bool {
    libraries().is_some()
}

const VERTEX_SHADER: &str = "
attribute vec2 position;
uniform vec2 viewport;
uniform vec4 destination;
uniform vec4 source;
varying vec2 coordinate;
void main() {
    vec2 pixel = destination.xy + position * destination.zw;
    gl_Position = vec4(pixel / viewport * 2.0 - 1.0, 0.0, 1.0);
    coordinate = source.xy + position * source.zw;
}
";

/// Premultiplied colour, optionally with red and blue swapped (shared
/// memory is uploaded as bytes, which GL reads as RGBA), optionally opaque,
/// and faded by `alpha`.
const FRAGMENT_BODY: &str = "
varying vec2 coordinate;
uniform float alpha;
uniform float swap;
uniform float opaque;
void main() {
    vec4 colour = texture2D(sampler, coordinate);
    colour = mix(colour, colour.bgra, swap);
    colour.a = mix(colour.a, 1.0, opaque);
    gl_FragColor = colour * alpha;
}
";

/// One linked program and its uniforms.
struct Program {
    id: u32,
    viewport: i32,
    destination: i32,
    source: i32,
    alpha: i32,
    swap: i32,
    opaque: i32,
}

struct Inner {
    egl: &'static EglFunctions,
    extensions: EglExtensions,
    gl: &'static GlFunctions,
    images: Option<GlImageFunctions>,
    dmabuf_queries: Option<EglDmabufQueries>,
    device_queries: Option<EglDeviceQueries>,
    display: Display,
    context: Context,
    textures: Program,
    external: Option<Program>,
    /// Whether each format and modifier can be sampled only as an external
    /// texture.
    external_only: RefCell<HashMap<(u32, u64), bool>>,
    /// Kept alive while EGL draws on it.
    _gbm: Option<gbm::Device>,
}

impl Inner {
    /// Makes the context current, if another thread's work has not.
    fn current(&self) {
        // Safety: the display and context are live.
        unsafe {
            if (self.egl.eglGetCurrentContext)() != self.context {
                (self.egl.eglMakeCurrent)(self.display, ptr::null_mut(), ptr::null_mut(), self.context);
            }
        }
    }

    fn egl_error(&self, what: &str) -> io::Error {
        // Safety: no arguments.
        let code = unsafe { (self.egl.eglGetError)() };
        io::Error::other(format!("{what} failed: EGL error {code:#x}"))
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Safety: the context and display are live, and every texture and
        // target that used them held this and is gone.
        unsafe {
            (self.gl.glDeleteProgram)(self.textures.id);
            if let Some(external) = &self.external {
                (self.gl.glDeleteProgram)(external.id);
            }
            (self.egl.eglMakeCurrent)(self.display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            (self.egl.eglDestroyContext)(self.display, self.context);
            (self.egl.eglTerminate)(self.display);
        }
    }
}

/// An EGL display and a GLES 2 context on it, current on this thread. A
/// clone is the same context, so textures one imports another draws.
#[derive(Clone)]
pub struct Gles(Rc<Inner>);

/// One plane of a dma-buf.
#[derive(Debug)]
pub struct DmabufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// A buffer in the GPU's memory, shared by descriptors.
#[derive(Debug)]
pub struct Dmabuf {
    pub width: i32,
    pub height: i32,
    /// A DRM fourcc.
    pub format: u32,
    pub modifier: u64,
    pub planes: Vec<DmabufPlane>,
}

/// A format and modifier EGL can import.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DmabufFormat {
    pub format: u32,
    pub modifier: u64,
    /// Sampled only as an external texture: it can be shown, not drawn into.
    pub external_only: bool,
}

/// A texture the renderer can draw.
pub struct Texture {
    gles: Rc<Inner>,
    id: u32,
    image: Option<Image>,
    external: bool,
    /// Uploaded from shared memory, whose bytes are BGRA.
    swap: bool,
    pub width: i32,
    pub height: i32,
}

/// What the renderer draws into.
pub struct Target {
    gles: Rc<Inner>,
    framebuffer: u32,
    renderbuffer: Option<u32>,
    texture: Option<u32>,
    image: Option<Image>,
    pub width: i32,
    pub height: i32,
}

fn has_extension(list: &str, name: &str) -> bool {
    list.split_ascii_whitespace().any(|extension| extension == name)
}

impl Gles {
    /// A context on a GBM device: the display's GPU, or a render node.
    pub fn on_gbm(device: &gbm::Device) -> io::Result<Gles> {
        Self::new(
            EGL_PLATFORM_GBM_KHR,
            device.raw(),
            "EGL_KHR_platform_gbm",
            Some(device.clone()),
        )
        .or_else(|_| {
            Self::new(
                EGL_PLATFORM_GBM_KHR,
                device.raw(),
                "EGL_MESA_platform_gbm",
                Some(device.clone()),
            )
        })
    }

    /// A context with no display of its own, as for tests: Mesa's software
    /// renderer, where there is no GPU.
    pub fn surfaceless() -> io::Result<Gles> {
        Self::new(
            EGL_PLATFORM_SURFACELESS_MESA,
            ptr::null_mut(),
            "EGL_MESA_platform_surfaceless",
            None,
        )
    }

    fn new(platform: u32, native: *mut c_void, needed: &str, gbm: Option<gbm::Device>) -> io::Result<Gles> {
        let libraries = libraries().ok_or_else(|| io::Error::other("libEGL or libGLESv2 could not be loaded"))?;
        let egl = &libraries.egl;
        let lookup = |name: &CStr| {
            // Safety: `name` is NUL-terminated.
            unsafe { (egl.eglGetProcAddress)(name.as_ptr()) }
        };
        // Safety: EGL_NO_DISPLAY asks for the client extensions.
        let client = unsafe { (egl.eglQueryString)(ptr::null_mut(), EGL_EXTENSIONS) };
        let client = if client.is_null() {
            String::new()
        } else {
            // Safety: EGL returns a NUL-terminated string it owns.
            unsafe { CStr::from_ptr(client) }.to_string_lossy().into_owned()
        };
        if !has_extension(&client, needed) || !has_extension(&client, "EGL_EXT_platform_base") {
            return Err(io::Error::other(format!("EGL has no {needed}")));
        }
        let extensions = EglExtensions::load(lookup).map_err(|name| io::Error::other(format!("EGL has no {name}")))?;
        // Safety: the native display is a live GBM device or null for
        // surfaceless, and the attribute list is empty.
        let display = unsafe { (extensions.eglGetPlatformDisplayEXT)(platform, native, [EGL_NONE].as_ptr()) };
        if display.is_null() {
            return Err(io::Error::other("eglGetPlatformDisplayEXT failed"));
        }
        let (mut major, mut minor) = (0, 0);
        // Safety: the display is valid and the out-pointers are live.
        if unsafe { (egl.eglInitialize)(display, &mut major, &mut minor) } != EGL_TRUE {
            return Err(io::Error::other("eglInitialize failed"));
        }
        // Safety: the display was initialized.
        let display_extensions = unsafe { CStr::from_ptr((egl.eglQueryString)(display, EGL_EXTENSIONS)) }
            .to_string_lossy()
            .into_owned();
        for needed in ["EGL_KHR_surfaceless_context", "EGL_KHR_image_base"] {
            if !has_extension(&display_extensions, needed) {
                // Safety: the display was initialized.
                unsafe { (egl.eglTerminate)(display) };
                return Err(io::Error::other(format!("the EGL display has no {needed}")));
            }
        }
        if !has_extension(&display_extensions, "EGL_KHR_no_config_context")
            && !has_extension(&display_extensions, "EGL_MESA_configless_context")
        {
            // Safety: the display was initialized.
            unsafe { (egl.eglTerminate)(display) };
            return Err(io::Error::other("the EGL display needs a config for a context"));
        }
        // Safety: a valid API.
        unsafe { (egl.eglBindAPI)(EGL_OPENGL_ES_API) };
        let attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        // Safety: the display was initialized; no config, no sharing.
        let context = unsafe { (egl.eglCreateContext)(display, ptr::null_mut(), ptr::null_mut(), attributes.as_ptr()) };
        if context.is_null() {
            // Safety: the display was initialized.
            unsafe { (egl.eglTerminate)(display) };
            return Err(io::Error::other("eglCreateContext failed"));
        }
        // Safety: surfaceless contexts need no surfaces.
        if unsafe { (egl.eglMakeCurrent)(display, ptr::null_mut(), ptr::null_mut(), context) } != EGL_TRUE {
            // Safety: both are live.
            unsafe {
                (egl.eglDestroyContext)(display, context);
                (egl.eglTerminate)(display);
            }
            return Err(io::Error::other("eglMakeCurrent failed"));
        }
        let gl = &libraries.gl;
        // Safety: the context is current.
        let gl_extensions = unsafe { CStr::from_ptr((gl.glGetString)(GL_EXTENSIONS).cast()) }
            .to_string_lossy()
            .into_owned();
        let images = has_extension(&gl_extensions, "GL_OES_EGL_image")
            .then(|| GlImageFunctions::load(lookup).ok())
            .flatten();
        let dmabuf_queries = has_extension(&display_extensions, "EGL_EXT_image_dma_buf_import_modifiers")
            .then(|| EglDmabufQueries::load(lookup).ok())
            .flatten();
        let device_queries = has_extension(&client, "EGL_EXT_device_query")
            .then(|| EglDeviceQueries::load(lookup).ok())
            .flatten();
        let compile = |prelude: &str, sampler: &str| -> io::Result<Program> {
            let fragment = format!("{prelude}precision mediump float;\nuniform {sampler} sampler;\n{FRAGMENT_BODY}");
            // Safety: the context is current.
            unsafe { link(gl, VERTEX_SHADER, &fragment) }
        };
        let textures = compile("", "sampler2D")?;
        let external = has_extension(&gl_extensions, "GL_OES_EGL_image_external")
            .then(|| compile("#extension GL_OES_EGL_image_external : require\n", "samplerExternalOES").ok())
            .flatten();
        Ok(Gles(Rc::new(Inner {
            egl,
            extensions,
            gl,
            images,
            dmabuf_queries,
            device_queries,
            display,
            context,
            textures,
            external,
            external_only: RefCell::new(HashMap::new()),
            _gbm: gbm,
        })))
    }

    /// The GPU the context draws with, as GL names it.
    pub fn renderer(&self) -> String {
        self.0.current();
        // Safety: the context is current.
        unsafe { CStr::from_ptr((self.0.gl.glGetString)(GL_RENDERER).cast()) }
            .to_string_lossy()
            .into_owned()
    }

    /// The render node of the device EGL draws on, where it says.
    pub fn render_node(&self) -> Option<PathBuf> {
        let queries = self.0.device_queries.as_ref()?;
        let mut device: isize = 0;
        // Safety: the display is live and the out-pointer is.
        if unsafe { (queries.eglQueryDisplayAttribEXT)(self.0.display, EGL_DEVICE_EXT, &mut device) } != EGL_TRUE {
            return None;
        }
        for name in [EGL_DRM_RENDER_NODE_FILE_EXT, EGL_DRM_DEVICE_FILE_EXT] {
            // Safety: the device came from EGL.
            let path = unsafe { (queries.eglQueryDeviceStringEXT)(device as *mut c_void, name) };
            if !path.is_null() {
                // Safety: EGL returns a NUL-terminated string it owns.
                let path = unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned();
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    /// Every dma-buf format and modifier EGL imports and this context can
    /// draw: one EGL samples only as an external texture is left out where GL
    /// has none.
    pub fn dmabuf_formats(&self) -> Vec<DmabufFormat> {
        let (Some(queries), Some(_)) = (&self.0.dmabuf_queries, &self.0.images) else {
            return Vec::new();
        };
        let display = self.0.display;
        let mut count = 0;
        // Safety: asking for the count alone.
        if unsafe { (queries.eglQueryDmaBufFormatsEXT)(display, 0, ptr::null_mut(), &mut count) } != EGL_TRUE {
            return Vec::new();
        }
        let mut formats = vec![0i32; count.max(0) as usize];
        // Safety: the vector holds `count` formats.
        unsafe { (queries.eglQueryDmaBufFormatsEXT)(display, count, formats.as_mut_ptr(), &mut count) };
        formats.truncate(count.max(0) as usize);
        let mut all = Vec::new();
        for format in formats {
            let mut count = 0;
            // Safety: asking for the count alone.
            unsafe {
                (queries.eglQueryDmaBufModifiersEXT)(display, format, 0, ptr::null_mut(), ptr::null_mut(), &mut count)
            };
            let mut modifiers = vec![0u64; count.max(0) as usize];
            let mut external = vec![0u32; count.max(0) as usize];
            // Safety: both vectors hold `count` entries.
            unsafe {
                (queries.eglQueryDmaBufModifiersEXT)(
                    display,
                    format,
                    count,
                    modifiers.as_mut_ptr(),
                    external.as_mut_ptr(),
                    &mut count,
                )
            };
            let count = count.max(0) as usize;
            if count == 0 {
                // Without modifiers, the driver chooses the layout.
                all.push(DmabufFormat {
                    format: format as u32,
                    modifier: gbm::MODIFIER_INVALID,
                    external_only: false,
                });
            }
            for index in 0..count.min(modifiers.len()) {
                all.push(DmabufFormat {
                    format: format as u32,
                    modifier: modifiers[index],
                    external_only: external[index] != 0,
                });
            }
        }
        let mut known = self.0.external_only.borrow_mut();
        for format in &all {
            known.insert((format.format, format.modifier), format.external_only);
        }
        let external = self.0.external.is_some();
        all.retain(|format| external || !format.external_only);
        all
    }

    /// Uploads premultiplied ARGB pixels from shared memory.
    pub fn upload(&self, width: i32, height: i32, pixels: &[u32]) -> io::Result<Texture> {
        if width <= 0 || height <= 0 || pixels.len() < (width as usize) * (height as usize) {
            return Err(io::Error::other("a texture's pixels do not fill it"));
        }
        let inner = &self.0;
        inner.current();
        let gl = inner.gl;
        let mut id = 0;
        // Safety: the context is current, and `pixels` holds width x height
        // pixels of four bytes, rows packed.
        unsafe {
            (gl.glGenTextures)(1, &mut id);
            (gl.glBindTexture)(GL_TEXTURE_2D, id);
            set_texture_parameters(gl, GL_TEXTURE_2D);
            (gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                width,
                height,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.as_ptr().cast(),
            );
        }
        Ok(Texture {
            gles: inner.clone(),
            id,
            image: None,
            external: false,
            swap: true,
            width,
            height,
        })
    }

    /// Replaces a rectangle of a texture uploaded from shared memory, from
    /// pixels `stride` wide.
    pub fn update(&self, texture: &Texture, pixels: &[u32], stride: i32, rect: (i32, i32, i32, i32)) {
        let (x, y, width, height) = rect;
        let fits = x >= 0
            && y >= 0
            && width > 0
            && height > 0
            && x + width <= texture.width
            && y + height <= texture.height
            && x + width <= stride
            && pixels.len() >= (stride as usize) * ((y + height) as usize);
        if !fits || texture.image.is_some() {
            return;
        }
        let rows: Vec<u32> = (y..y + height)
            .flat_map(|row| {
                let start = (row * stride + x) as usize;
                pixels[start..start + width as usize].iter().copied()
            })
            .collect();
        let inner = &self.0;
        inner.current();
        // Safety: the context is current, the texture is live, and `rows`
        // holds width x height pixels.
        unsafe {
            (inner.gl.glBindTexture)(GL_TEXTURE_2D, texture.id);
            (inner.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
            (inner.gl.glTexSubImage2D)(
                GL_TEXTURE_2D,
                0,
                x,
                y,
                width,
                height,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                rows.as_ptr().cast(),
            );
        }
    }

    fn create_image(&self, dmabuf: &Dmabuf) -> io::Result<Image> {
        let inner = &self.0;
        if dmabuf.planes.is_empty() || dmabuf.planes.len() > 4 {
            return Err(io::Error::other("a dma-buf has one to four planes"));
        }
        let mut attributes = vec![
            EGL_WIDTH,
            dmabuf.width,
            EGL_HEIGHT,
            dmabuf.height,
            EGL_LINUX_DRM_FOURCC_EXT,
            dmabuf.format as i32,
        ];
        for (plane, names) in dmabuf.planes.iter().zip(PLANE_ATTRIBUTES) {
            attributes.extend([
                names[0],
                plane.fd.as_raw_fd(),
                names[1],
                plane.offset as i32,
                names[2],
                plane.stride as i32,
            ]);
            if dmabuf.modifier != gbm::MODIFIER_INVALID {
                attributes.extend([
                    names[3],
                    (dmabuf.modifier & 0xffff_ffff) as i32,
                    names[4],
                    (dmabuf.modifier >> 32) as i32,
                ]);
            }
        }
        attributes.extend([EGL_IMAGE_PRESERVED_KHR, 1, EGL_NONE]);
        // Safety: the display is live, and the attributes name descriptors
        // that stay open until the image is made.
        let image = unsafe {
            (inner.extensions.eglCreateImageKHR)(
                inner.display,
                ptr::null_mut(),
                EGL_LINUX_DMA_BUF_EXT,
                ptr::null_mut(),
                attributes.as_ptr(),
            )
        };
        if image.is_null() {
            return Err(inner.egl_error("importing a dma-buf"));
        }
        Ok(image)
    }

    /// A texture that shows a dma-buf.
    pub fn import(&self, dmabuf: &Dmabuf) -> io::Result<Texture> {
        let inner = &self.0;
        let images = inner
            .images
            .as_ref()
            .ok_or_else(|| io::Error::other("GL cannot use EGL images"))?;
        inner.current();
        let external_only = inner
            .external_only
            .borrow()
            .get(&(dmabuf.format, dmabuf.modifier))
            .copied()
            .unwrap_or(false);
        if external_only && inner.external.is_none() {
            return Err(io::Error::other(
                "the dma-buf needs external textures, which GL has not",
            ));
        }
        let image = self.create_image(dmabuf)?;
        let target = if external_only {
            GL_TEXTURE_EXTERNAL_OES
        } else {
            GL_TEXTURE_2D
        };
        let mut id = 0;
        // Safety: the context is current and the image is live.
        unsafe {
            (inner.gl.glGenTextures)(1, &mut id);
            (inner.gl.glBindTexture)(target, id);
            set_texture_parameters(inner.gl, target);
            (images.glEGLImageTargetTexture2DOES)(target, image);
        }
        Ok(Texture {
            gles: inner.clone(),
            id,
            image: Some(image),
            external: external_only,
            swap: false,
            width: dmabuf.width,
            height: dmabuf.height,
        })
    }

    /// A target that draws into a dma-buf, as a display's buffer.
    pub fn target(&self, dmabuf: &Dmabuf) -> io::Result<Target> {
        let inner = &self.0;
        let images = inner
            .images
            .as_ref()
            .ok_or_else(|| io::Error::other("GL cannot use EGL images"))?;
        inner.current();
        let image = self.create_image(dmabuf)?;
        let gl = inner.gl;
        let (mut framebuffer, mut renderbuffer) = (0, 0);
        // Safety: the context is current and the image is live.
        let status = unsafe {
            (gl.glGenRenderbuffers)(1, &mut renderbuffer);
            (gl.glBindRenderbuffer)(GL_RENDERBUFFER, renderbuffer);
            (images.glEGLImageTargetRenderbufferStorageOES)(GL_RENDERBUFFER, image);
            (gl.glGenFramebuffers)(1, &mut framebuffer);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, framebuffer);
            (gl.glFramebufferRenderbuffer)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, renderbuffer);
            (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER)
        };
        let target = Target {
            gles: inner.clone(),
            framebuffer,
            renderbuffer: Some(renderbuffer),
            texture: None,
            image: Some(image),
            width: dmabuf.width,
            height: dmabuf.height,
        };
        if status != GL_FRAMEBUFFER_COMPLETE {
            return Err(io::Error::other(format!("a dma-buf cannot be drawn into: {status:#x}")));
        }
        Ok(target)
    }

    /// A target in a texture of the GPU's own, as for tests.
    pub fn offscreen(&self, width: i32, height: i32) -> io::Result<Target> {
        let inner = &self.0;
        inner.current();
        let gl = inner.gl;
        let (mut framebuffer, mut texture) = (0, 0);
        // Safety: the context is current.
        let status = unsafe {
            (gl.glGenTextures)(1, &mut texture);
            (gl.glBindTexture)(GL_TEXTURE_2D, texture);
            set_texture_parameters(gl, GL_TEXTURE_2D);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                width,
                height,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            (gl.glGenFramebuffers)(1, &mut framebuffer);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, framebuffer);
            (gl.glFramebufferTexture2D)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
            (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER)
        };
        let target = Target {
            gles: inner.clone(),
            framebuffer,
            renderbuffer: None,
            texture: Some(texture),
            image: None,
            width,
            height,
        };
        if status != GL_FRAMEBUFFER_COMPLETE {
            return Err(io::Error::other(format!(
                "an offscreen target is incomplete: {status:#x}"
            )));
        }
        Ok(target)
    }

    /// Starts drawing into a target.
    pub fn frame<'a>(&'a self, target: &'a Target) -> Frame<'a> {
        let inner = &self.0;
        inner.current();
        // Safety: the context is current and the framebuffer is live.
        unsafe {
            (inner.gl.glBindFramebuffer)(GL_FRAMEBUFFER, target.framebuffer);
            (inner.gl.glViewport)(0, 0, target.width, target.height);
            (inner.gl.glEnable)(GL_SCISSOR_TEST);
        }
        Frame { inner, target }
    }
}

/// Drawing into one target.
pub struct Frame<'a> {
    inner: &'a Inner,
    target: &'a Target,
}

/// A rectangle in pixels: x, y, width, height.
pub type Rect = (i32, i32, i32, i32);

impl Frame<'_> {
    fn scissor(&self, clip: Rect) -> bool {
        let (x, y, width, height) = clip;
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + width).min(self.target.width);
        let y1 = (y + height).min(self.target.height);
        if x1 <= x0 || y1 <= y0 {
            return false;
        }
        // Safety: the context is current.
        unsafe { (self.inner.gl.glScissor)(x0, y0, x1 - x0, y1 - y0) };
        true
    }

    /// Fills a rectangle with one premultiplied ARGB colour.
    pub fn clear(&mut self, rect: Rect, argb: u32) {
        if !self.scissor(rect) {
            return;
        }
        let channel = |shift: u32| ((argb >> shift) & 0xff) as f32 / 255.0;
        // Safety: the context is current.
        unsafe {
            (self.inner.gl.glClearColor)(channel(16), channel(8), channel(0), channel(24));
            (self.inner.gl.glClear)(GL_COLOR_BUFFER_BIT);
        }
    }

    /// Draws `source`, a rectangle of the texture in its pixels, onto
    /// `destination`, only where it falls inside `clip`. `opaque` ignores
    /// the texture's alpha, as for a buffer with no alpha channel.
    pub fn draw(
        &mut self,
        texture: &Texture,
        source: (f32, f32, f32, f32),
        destination: Rect,
        clip: Rect,
        opaque: bool,
        alpha: f32,
    ) {
        let program = match (texture.external, &self.inner.external) {
            (false, _) => &self.inner.textures,
            (true, Some(external)) => external,
            (true, None) => return,
        };
        if !self.scissor(clip) || texture.width <= 0 || texture.height <= 0 {
            return;
        }
        let gl = self.inner.gl;
        let target_kind = if texture.external {
            GL_TEXTURE_EXTERNAL_OES
        } else {
            GL_TEXTURE_2D
        };
        let quad: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let (width, height) = (texture.width as f32, texture.height as f32);
        // Safety: the context is current, the texture and program are live,
        // and `quad` outlives the draw that reads it.
        unsafe {
            (gl.glUseProgram)(program.id);
            (gl.glActiveTexture)(GL_TEXTURE0);
            (gl.glBindTexture)(target_kind, texture.id);
            (gl.glUniform2f)(program.viewport, self.target.width as f32, self.target.height as f32);
            (gl.glUniform4f)(
                program.destination,
                destination.0 as f32,
                destination.1 as f32,
                destination.2 as f32,
                destination.3 as f32,
            );
            (gl.glUniform4f)(
                program.source,
                source.0 / width,
                source.1 / height,
                source.2 / width,
                source.3 / height,
            );
            (gl.glUniform1f)(program.alpha, alpha);
            (gl.glUniform1f)(program.swap, if texture.swap { 1.0 } else { 0.0 });
            (gl.glUniform1f)(program.opaque, if opaque { 1.0 } else { 0.0 });
            if opaque && alpha >= 1.0 {
                (gl.glDisable)(GL_BLEND);
            } else {
                (gl.glEnable)(GL_BLEND);
                (gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            }
            (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, 0, 0, quad.as_ptr().cast());
            (gl.glEnableVertexAttribArray)(0);
            (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
        }
    }

    /// Reads a rectangle back as premultiplied ARGB, rows from the top.
    pub fn read(&self, rect: Rect) -> Vec<u32> {
        let (x, y, width, height) = rect;
        let mut bytes = vec![0u8; (width.max(0) as usize) * (height.max(0) as usize) * 4];
        // Safety: the context is current, and `bytes` holds the rectangle.
        unsafe {
            (self.inner.gl.glPixelStorei)(GL_PACK_ALIGNMENT, 4);
            (self.inner.gl.glReadPixels)(
                x,
                y,
                width,
                height,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                bytes.as_mut_ptr().cast(),
            );
        }
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|[red, green, blue, alpha]| u32::from_be_bytes([*alpha, *red, *green, *blue]))
            .collect()
    }

    /// Waits until the GPU has drawn everything, so the display may show it.
    pub fn finish(self) {
        // Safety: the context is current.
        unsafe { (self.inner.gl.glFinish)() };
    }
}

/// Clamped edges, and linear filtering when scaled.
///
/// # Safety
///
/// The context must be current, with the texture bound to `target`.
unsafe fn set_texture_parameters(gl: &GlFunctions, target: u32) {
    // Safety: as the caller promises.
    unsafe {
        (gl.glTexParameteri)(target, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (gl.glTexParameteri)(target, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (gl.glTexParameteri)(target, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (gl.glTexParameteri)(target, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
}

/// Compiles and links a program, with `position` at attribute 0.
///
/// # Safety
///
/// The context must be current.
unsafe fn link(gl: &GlFunctions, vertex: &str, fragment: &str) -> io::Result<Program> {
    // Safety: as the caller promises; every pointer passed lives through the
    // call.
    unsafe {
        let compile = |kind: u32, source: &str| -> io::Result<u32> {
            let shader = (gl.glCreateShader)(kind);
            let source = CString::new(source).map_err(io::Error::other)?;
            let sources = [source.as_ptr()];
            (gl.glShaderSource)(shader, 1, sources.as_ptr(), ptr::null());
            (gl.glCompileShader)(shader);
            let mut ok = 0;
            (gl.glGetShaderiv)(shader, GL_COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut log = vec![0 as c_char; 1024];
                (gl.glGetShaderInfoLog)(shader, log.len() as i32, ptr::null_mut(), log.as_mut_ptr());
                (gl.glDeleteShader)(shader);
                let message = CStr::from_ptr(log.as_ptr()).to_string_lossy().into_owned();
                return Err(io::Error::other(format!("a shader did not compile: {message}")));
            }
            Ok(shader)
        };
        let vertex = compile(GL_VERTEX_SHADER, vertex)?;
        let fragment = match compile(GL_FRAGMENT_SHADER, fragment) {
            Ok(shader) => shader,
            Err(error) => {
                (gl.glDeleteShader)(vertex);
                return Err(error);
            }
        };
        let program = (gl.glCreateProgram)();
        (gl.glAttachShader)(program, vertex);
        (gl.glAttachShader)(program, fragment);
        (gl.glBindAttribLocation)(program, 0, c"position".as_ptr());
        (gl.glLinkProgram)(program);
        (gl.glDeleteShader)(vertex);
        (gl.glDeleteShader)(fragment);
        let mut ok = 0;
        (gl.glGetProgramiv)(program, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut log = vec![0 as c_char; 1024];
            (gl.glGetProgramInfoLog)(program, log.len() as i32, ptr::null_mut(), log.as_mut_ptr());
            (gl.glDeleteProgram)(program);
            let message = CStr::from_ptr(log.as_ptr()).to_string_lossy().into_owned();
            return Err(io::Error::other(format!("a program did not link: {message}")));
        }
        let uniform = |name: &CStr| (gl.glGetUniformLocation)(program, name.as_ptr());
        (gl.glUseProgram)(program);
        (gl.glUniform1i)(uniform(c"sampler"), 0);
        Ok(Program {
            id: program,
            viewport: uniform(c"viewport"),
            destination: uniform(c"destination"),
            source: uniform(c"source"),
            alpha: uniform(c"alpha"),
            swap: uniform(c"swap"),
            opaque: uniform(c"opaque"),
        })
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        self.gles.current();
        // Safety: the context is current; the texture and image are this
        // one's.
        unsafe {
            (self.gles.gl.glDeleteTextures)(1, &self.id);
            if let Some(image) = self.image {
                (self.gles.extensions.eglDestroyImageKHR)(self.gles.display, image);
            }
        }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        self.gles.current();
        // Safety: the context is current; these objects are this target's.
        unsafe {
            (self.gles.gl.glDeleteFramebuffers)(1, &self.framebuffer);
            if let Some(renderbuffer) = self.renderbuffer {
                (self.gles.gl.glDeleteRenderbuffers)(1, &renderbuffer);
            }
            if let Some(texture) = self.texture {
                (self.gles.gl.glDeleteTextures)(1, &texture);
            }
            if let Some(image) = self.image {
                (self.gles.extensions.eglDestroyImageKHR)(self.gles.display, image);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mesa's surfaceless platform draws with its software renderer where
    /// there is no GPU, so these run on CI. Where EGL cannot be loaded at
    /// all, they say so and pass.
    fn gles() -> Option<Gles> {
        match Gles::surfaceless() {
            Ok(gles) => Some(gles),
            Err(error) => {
                eprintln!("no EGL: skipped ({error})");
                None
            }
        }
    }

    #[test]
    fn a_shared_memory_texture_draws_its_own_colours_where_it_is_put() {
        let Some(gles) = gles() else { return };
        // Red over blue, premultiplied ARGB.
        let pixels: Vec<u32> = (0..4 * 4)
            .map(|index| if index < 8 { 0xffff_0000 } else { 0xff00_00ff })
            .collect();
        let texture = gles.upload(4, 4, &pixels).unwrap();
        let target = gles.offscreen(16, 16).unwrap();

        let mut frame = gles.frame(&target);
        frame.clear((0, 0, 16, 16), 0xff00_ff00);
        frame.draw(&texture, (0.0, 0.0, 4.0, 4.0), (4, 4, 8, 8), (0, 0, 16, 16), false, 1.0);
        let read = frame.read((0, 0, 16, 16));
        frame.finish();

        let at = |x: usize, y: usize| read[y * 16 + x];
        assert_eq!(at(0, 0), 0xff00_ff00, "the clear colour around it");
        assert_eq!(at(6, 5), 0xffff_0000, "the top half, at the top");
        assert_eq!(at(6, 10), 0xff00_00ff, "the bottom half, below");
        assert_eq!(at(12, 12), 0xff00_ff00);
    }

    #[test]
    fn a_draw_stays_inside_its_clip_and_blends_premultiplied() {
        let Some(gles) = gles() else { return };
        // Half-transparent white, premultiplied.
        let texture = gles.upload(1, 1, &[0x8080_8080]).unwrap();
        let target = gles.offscreen(8, 8).unwrap();

        let mut frame = gles.frame(&target);
        frame.clear((0, 0, 8, 8), 0xff00_0000);
        frame.draw(&texture, (0.0, 0.0, 1.0, 1.0), (0, 0, 8, 8), (0, 0, 4, 8), false, 1.0);
        let read = frame.read((0, 0, 8, 8));
        frame.finish();

        let grey = read[0];
        assert!(
            (0x7f..=0x81).contains(&((grey >> 16) & 0xff)),
            "white at half over black is half grey: {grey:#010x}"
        );
        assert_eq!(read[5], 0xff00_0000, "outside the clip nothing is drawn");
    }

    #[test]
    fn an_update_replaces_only_its_rectangle() {
        let Some(gles) = gles() else { return };
        let texture = gles.upload(4, 4, &[0xff00_0000; 16]).unwrap();
        gles.update(&texture, &[0xffff_ffff; 16], 4, (1, 1, 2, 2));
        let target = gles.offscreen(4, 4).unwrap();

        let mut frame = gles.frame(&target);
        frame.draw(&texture, (0.0, 0.0, 4.0, 4.0), (0, 0, 4, 4), (0, 0, 4, 4), true, 1.0);
        let read = frame.read((0, 0, 4, 4));
        frame.finish();

        assert_eq!(read[0], 0xff00_0000);
        assert_eq!(read[5], 0xffff_ffff);
        assert_eq!(read[10], 0xffff_ffff);
        assert_eq!(read[15], 0xff00_0000);
    }

    const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

    /// Reads back what a texture shows, drawn opaque onto 8x8.
    fn shown(gles: &Gles, texture: &Texture) -> Vec<u32> {
        let target = gles.offscreen(8, 8).unwrap();
        let mut frame = gles.frame(&target);
        frame.clear((0, 0, 8, 8), 0xff00_0000);
        let size = (texture.width as f32, texture.height as f32);
        frame.draw(
            texture,
            (0.0, 0.0, size.0, size.1),
            (0, 0, 8, 8),
            (0, 0, 8, 8),
            true,
            1.0,
        );
        let read = frame.read((0, 0, 8, 8));
        frame.finish();
        read
    }

    #[test]
    fn a_linear_dma_buf_in_shared_memory_shows_its_own_colours() {
        let Some(gles) = gles() else { return };
        let supported = gles
            .dmabuf_formats()
            .iter()
            .any(|format| format.format == XRGB8888 && format.modifier == 0);
        if !supported {
            eprintln!("EGL imports no linear XR24 dma-bufs: skipped");
            return;
        }
        let fd = rustix::fs::memfd_create("dmabuf", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let bytes: Vec<u8> = std::iter::repeat_n(0xff20_40c0u32.to_le_bytes(), 8 * 8)
            .flatten()
            .collect();
        rustix::io::write(&fd, &bytes).unwrap();
        let dmabuf = Dmabuf {
            width: 8,
            height: 8,
            format: XRGB8888,
            modifier: 0,
            planes: vec![DmabufPlane {
                fd,
                offset: 0,
                stride: 32,
            }],
        };
        let texture = match gles.import(&dmabuf) {
            Ok(texture) => texture,
            Err(error) => {
                eprintln!("EGL takes no memfd as a dma-buf: skipped ({error})");
                return;
            }
        };
        assert_eq!(shown(&gles, &texture)[9], 0xff20_40c0);
    }

    /// A render node's GPU, through GBM.
    fn gpu() -> Option<(gbm::Device, Gles)> {
        let path = std::fs::read_dir("/dev/dri")
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("renderD"))
            })?;
        let fd: OwnedFd = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .ok()?
            .into();
        let device = gbm::Device::new(fd).ok()?;
        let gles = Gles::on_gbm(&device).ok()?;
        Some((device, gles))
    }

    #[test]
    fn a_buffer_in_the_gpus_own_layout_shows_what_was_drawn_into_it() {
        let Some((device, gles)) = gpu() else {
            eprintln!("no GPU: skipped");
            return;
        };
        let modifiers: Vec<u64> = gles
            .dmabuf_formats()
            .iter()
            .filter(|format| format.format == XRGB8888 && !format.external_only)
            .map(|format| format.modifier)
            .collect();
        let buffer = device
            .create_buffer(8, 8, XRGB8888, &modifiers, gbm::USE_RENDERING)
            .unwrap();
        let dmabuf = || Dmabuf {
            width: 8,
            height: 8,
            format: XRGB8888,
            modifier: buffer.modifier(),
            planes: buffer
                .planes()
                .iter()
                .enumerate()
                .map(|(index, plane)| DmabufPlane {
                    fd: buffer.export(index).unwrap(),
                    offset: plane.offset,
                    stride: plane.stride,
                })
                .collect(),
        };
        eprintln!("modifier {:#x}, {} planes", buffer.modifier(), buffer.planes().len());
        let target = gles.target(&dmabuf()).unwrap();
        let mut frame = gles.frame(&target);
        frame.clear((0, 0, 8, 8), 0xff20_40c0);
        frame.finish();

        let texture = gles.import(&dmabuf()).unwrap();

        assert_eq!(shown(&gles, &texture)[9], 0xff20_40c0);
    }
}
