Name:           crowbaros-mesa-intel-virtio
Version:        26.2.6
Release:        1%{?dist}
Summary:        Isolated Intel VirtIO Vulkan driver for the CrowbarOS VM lab
License:        MIT AND BSD-3-Clause AND SGI-B-2.0
URL:            https://mesa3d.org
Source0:        https://archive.mesa3d.org/mesa-%{version}.tar.xz
ExclusiveArch:  x86_64
BuildRequires:  gcc gcc-c++ meson ninja-build pkgconf-pkg-config bison flex
BuildRequires:  python3-mako python3-pycparser python3-pyyaml python3-packaging
BuildRequires:  pkgconfig(libdrm) >= 2.4.133
BuildRequires:  pkgconfig(expat) pkgconfig(zlib) pkgconfig(libzstd) pkgconfig(libelf)
BuildRequires:  pkgconfig(wayland-scanner) pkgconfig(wayland-protocols)
BuildRequires:  pkgconfig(wayland-client) pkgconfig(libdisplay-info)
BuildRequires:  llvm-devel clang-devel pkgconfig(LLVMSPIRVLib) pkgconfig(SPIRV-Tools)
BuildRequires:  vulkan-headers glslang
Requires:       vulkan-loader

%global debug_package %{nil}
%global _lto_cflags %{nil}
%global lab_prefix /opt/crowbaros-lab/mesa-intel-virtio

%description
Upstream Mesa's Intel Vulkan driver with the experimental VirtIO native-context
backend enabled. This development-only package installs to a private lab prefix.
It does not replace Fedora Mesa or register a global Vulkan driver. Select it
explicitly with VK_DRIVER_FILES for testing on the supported Intel i915 host.

%prep
%autosetup -n mesa-%{version}

%build
meson setup build --prefix=%{lab_prefix} --libdir=lib64 \
  --buildtype=release --wrap-mode=nodownload \
  -Dvulkan-drivers=intel -Dintel-virtio-experimental=true \
  -Dgallium-drivers= -Dplatforms=wayland -Dglx=disabled -Degl=disabled \
  -Dgbm=disabled -Dopengl=false -Dgles1=disabled -Dgles2=disabled \
  -Dllvm=enabled -Dshared-llvm=enabled -Dintel-rt=disabled \
  -Dvideo-codecs= -Dbuild-tests=false -Dvalgrind=disabled -Dlibunwind=disabled
meson compile -C build -j 6

%install
DESTDIR=%{buildroot} meson install -C build --no-rebuild

%check
python3 - <<'PY'
import json, pathlib
root = pathlib.Path('%{buildroot}%{lab_prefix}')
icd = json.loads((root / 'share/vulkan/icd.d/intel_icd.x86_64.json').read_text())
assert icd['ICD']['library_path'] == '%{lab_prefix}/lib64/libvulkan_intel.so'
assert (root / 'lib64/libvulkan_intel.so').is_file()
PY

%files
%license docs/license.rst
%{lab_prefix}/

%changelog
* Sat Nov 14 2026 CrowbarOS <local-build@crowbaros.invalid> - 26.2.6-1
- Enable Intel VirtIO backend in a separate lab-only Vulkan driver prefix.
