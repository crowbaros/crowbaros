"""Typeset the CrowbarOS brand guide. Run with ReportLab and Pillow installed."""
from pathlib import Path
import json, math
from PIL import Image
from reportlab.pdfgen import canvas
from reportlab.lib.colors import HexColor
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.lib.utils import ImageReader

ROOT=Path(__file__).resolve().parents[1]
W,H=1280,800
C={'void':'#080D12','abyss':'#17212B','ice':'#49D7E8','steel':'#6D8191','frost':'#D8E5EC','muted':'#A8BAC7','line':'#364957'}
pdfmetrics.registerFont(TTFont('Manrope','/tmp/Crowbar-Manrope-Regular.ttf'))
pdfmetrics.registerFont(TTFont('ManropeBold','/tmp/Crowbar-Manrope-Bold.ttf'))
pdfmetrics.registerFont(TTFont('Plex',str(ROOT/'fonts/IBMPlexMono-Regular.ttf')))
out=ROOT/'guide/CrowbarOS-brand-guide.pdf';p=canvas.Canvas(str(out),pagesize=(W,H),pageCompression=1)
p.setTitle('CrowbarOS — Brand kit 1.0');p.setAuthor('CrowbarOS');p.setSubject('Identity, logo usage, palette, typography, mascot, wallpapers and desktop design concepts')
page=0
def color(v):return HexColor(C.get(v,v))
def rect(x,y,w,h,c,r=0,stroke=None):
 p.setFillColor(color(c));p.setStrokeColor(color(stroke or c));p.setLineWidth(.75)
 if r:p.roundRect(x,H-y-h,w,h,r,fill=1,stroke=bool(stroke))
 else:p.rect(x,H-y-h,w,h,fill=1,stroke=bool(stroke))
def text(x,y,s,size=16,c='frost',font='Manrope'):
 p.setFont(font,size);p.setFillColor(color(c));p.drawString(x,H-y-size*.83,s)
def para(x,y,s,width=500,size=17,c='muted',leading=None,font='Manrope'):
 leading=leading or size*1.55
 for paragraph in s.split('\n'):
  line=''
  for w in paragraph.split():
   test=(line+' '+w).strip()
   if pdfmetrics.stringWidth(test,font,size)>width and line:text(x,y,line,size,c,font);y+=leading;line=w
   else:line=test
  if line:text(x,y,line,size,c,font);y+=leading
  y+=leading*.35
 return y
def img(path,x,y,w,h,fit=True):
 path=ROOT/path
 iw,ih=Image.open(path).size
 if fit:
  scale=min(w/iw,h/ih);dw,dh=iw*scale,ih*scale;x+=(w-dw)/2;y+=(h-dh)/2;w,h=dw,dh
 p.drawImage(ImageReader(str(path)),x,H-y-h,w,h,mask='auto')
def line(x1,y1,x2,y2,c='line',dash=None):
 p.setStrokeColor(color(c));p.setLineWidth(.75);p.setDash(dash or []);p.line(x1,H-y1,x2,H-y2);p.setDash([])
def begin(title=None,sub=None):
 global page
 if page:p.showPage()
 page+=1;rect(0,0,W,H,'void')
 if title:text(60,42,title,34,'frost','ManropeBold')
 if sub:para(60,92,sub,1160,15)
 text(60,765,'CrowbarOS / Brand kit 1.0',11,'muted');text(1176,765,f'{page:02}',11,'muted')
def logo(x,y,w=600,variant='primary',kind='horizontal'):
 img(f'logos/png/crowbaros-{kind}-{variant}@2x.png',x,y,w,w*(180/850 if kind=='horizontal' else 420/680),True)
def luminance(h):
 v=[int(h[i:i+2],16)/255 for i in (1,3,5)];v=[x/12.92 if x<=.04045 else ((x+.055)/1.055)**2.4 for x in v];return .2126*v[0]+.7152*v[1]+.0722*v[2]
def contrast(a,b):
 v=sorted([luminance(a),luminance(b)]);return (v[1]+.05)/(v[0]+.05)
ratios=[('Frost / Void',contrast(C['frost'],C['void'])),('Frost / Abyss',contrast(C['frost'],C['abyss'])),('Ice / Void',contrast(C['ice'],C['void'])),('Ice / Abyss',contrast(C['ice'],C['abyss'])),('Steel / Abyss',contrast(C['steel'],C['abyss'])),('Muted / Abyss',contrast(C['muted'],C['abyss']))]
(ROOT/'tokens/contrast-ratios.json').write_text(json.dumps(dict(ratios),indent=2)+'\n')

begin();img('wallpapers/nightwatch.png',0,0,1280,721,False);logo(68,178,700);para(79,365,'Quiet surface.\nSharp mind.',600,44,'frost',54);text(80,625,'Brand identity and desktop studies',19);text(80,660,'Version 1.0 · November 2026',14,'muted')

begin('A quiet system with a sharp identity.','A complete expression of the selected CrowbarOS direction: Nullkeel’s colors and Rookslash’s crow.')
para(60,167,'CrowbarOS pairs a restrained technical surface with the intelligence and character of a crow. The engraving carries texture; the interface carries clarity.',510,25,'frost',36)
for y,head,body in [(355,'Curious','Invite people to explore and make the desktop their own.'),(454,'Capable','Show precision through useful controls, clear text and quiet feedback.'),(553,'Independent','Respect user choice. Avoid fear, gatekeeping and hacker theatrics.')]:
 text(60,y,head,22,'frost','ManropeBold');para(60,y+38,body,480,17)
img('social/crowbaros-social-card-1200x630.png',635,173,585,307)
para(650,530,'Brand line',520,15,'ice');para(650,563,'Quiet surface. Sharp mind.',520,28,'frost')
para(650,625,'Descriptor: A community-built Linux desktop. This is the identity for a planned distribution; the mockups do not imply a released OS.',510,16)

begin('The logo system.','The slash preserves the original mark’s edge. A simplified crow head keeps the identity readable at small sizes.')
logo(110,164,1040)
line(60,430,1220,430)
rect(60,475,360,182,'abyss',12);logo(78,516,325,'frost')
rect(460,475,360,182,'frost',12);logo(478,516,325,'dark')
rect(860,475,360,182,'void',12,'line');logo(878,516,325,'ice')
text(60,681,'Frost / single color',15);text(460,681,'Void / light surfaces',15);text(860,681,'Ice / accent use',15)
para(60,718,'SVG masters contain outlined lettering. Four colorways each include mark, horizontal, stacked and wordmark-only versions.',1160,14)

begin('Give the mark space.','Use the supplied masters. Do not recreate the wordmark by typing the name in a similar font.')
rect(60,167,720,300,'abyss',12);logo(130,245,560)
for a,b,c,d in [(108,210,730,210),(108,402,730,402),(108,210,108,402),(730,210,730,402)]:line(a,b,c,d,'steel',[5,5])
text(130,181,'Clear space = ¼ of the visible symbol height',14,'frost')
para(60,500,'Use primary cyan + Frost on Void or Abyss. Use the all-Void mark on light surfaces. Keep all logo elements in the same proportions and orientation.',710,19)
para(60,617,'Avoid gradients in the mark, outlines, drop shadows, extra feathers, changed slash angles, squashed proportions, or a logo placed over busy feather detail.',710,18)
text(850,178,'Minimum digital sizes',23,'frost','ManropeBold')
for y,label,desc in [(236,'Horizontal: 160 px wide','Prefer 240 px or more in headers.'),(320,'Stacked: 140 px wide','Use when the frame is nearly square.'),(404,'Standard symbol: 24 px','Use the micro symbol at 16–23 px.')]:
 text(850,y,label,18);para(850,y+31,desc,350,15)
img('logos/png/crowbaros-mark-24.png',860,530,24,24);img('logos/png/crowbaros-mark-48.png',922,518,48,48);img('logos/png/crowbaros-mark-128.png',1013,480,128,128)
para(850,640,'Small PNG exports are supplied at 16–1024 px. App icons include a solid Void ground; all other logo PNGs have alpha.',350,15)

begin('Five colors. One controlled accent.','The five original Nullkeel values are preserved exactly in the SVG masters, color swatches and design tokens.')
for i,(name,key,role) in enumerate([('Void','void','Canvas'),('Abyss','abyss','Raised surface'),('Ice','ice','Focus + action'),('Steel','steel','Decoration'),('Frost','frost','Primary text')]):
 x=60+i*236;rect(x,161,216,154,key,12,'line' if key=='void' else None);text(x,337,name,24,'frost','ManropeBold');text(x,376,C[key],18,'muted','Plex');rgb=' / '.join(str(int(C[key][n:n+2],16)) for n in (1,3,5));text(x,410,rgb,14,'muted','Plex');text(x,443,role,15)
para(60,510,'Let dark surfaces dominate. Use Ice for one primary signal at a time: active window, selected control or a feather accent. Frost supplies legibility.',540,19)
para(60,627,'Steel is not normal body text on Abyss. Use the derived UI secondary color #A8BAC7 instead. This is a functional support token, not a sixth brand swatch.',540,17)
text(700,510,'Contrast checks',20,'frost','ManropeBold')
for i,(label,ratio) in enumerate(ratios):
 y=552+i*28;text(700,y,label,15,'muted');text(1042,y,f'{ratio:.2f}:1',15,'frost','Plex');text(1147,y,'AA' if ratio>=4.5 else 'Fail*',13,'ice' if ratio>=4.5 else 'frost')
text(700,725,'*Normal-text target: 4.5:1. Pair color with a shape or label.',12,'muted')

begin('Type that stays human.','Proportional typography belongs in the interface. Monospace earns its place in terminals, paths and technical data.')
text(60,166,'Manrope',68,'frost','ManropeBold');para(60,265,'ABCDEFGHIJKLMNOPQRSTUVWXYZ\nabcdefghijklmnopqrstuvwxyz\n0123456789',600,28,'frost',43)
text(60,441,'A place to make things.',34,'frost','ManropeBold');para(60,503,'UI text 14–17 px / comfortable line height. Headings 24–42 px / weight 600–700. Use sentence case, short labels and calm, direct language.',570,19)
text(740,178,'IBM Plex Mono',31,'ice','Plex');para(740,240,'~/Projects/CrowbarOS\n$ make something\n#49D7E8',480,22,'frost',39,'Plex')
text(740,430,'Voice',24,'frost','ManropeBold');para(740,478,'Say: “Make it yours.”\nSay: “Your changes are saved.”\nSay: “Find your tools.”\nAvoid: elite-only slogans, military language and unsupported security promises.',455,18)
para(60,678,'Both font families and their SIL Open Font License files are included. Keep the outlined logo as the identity master; ordinary UI text can use the variable Manrope font.',1120,16)

begin('Meet the crow.','An observant, independent character. It is brand artwork, never an assistant or a notification messenger.')
img('mascot/crow-master.png',68,150,405,595)
text(555,174,'The recognizable details',26,'frost','ManropeBold')
para(555,222,'A natural right-facing crow, folded wing, slate perch and precise engraved feather structure. One cyan wing feather links the mascot to the slash and the wallpaper family.',620,21)
para(555,365,'Keep the whole silhouette for hero placements. Leave the head and beak clear. Do not mirror, recolor individual feathers, add clothing or turn the eye into a glowing device.',620,18)
para(555,482,'Use the simple vector symbol below roughly 180 px of mascot height. Feather engraving belongs at generous sizes where the texture can breathe.',620,18)
rect(555,592,665,120,'abyss',12);text(579,615,'Native transparent PNG · 1024 × 1536',20);para(579,650,'Verified on Void, Abyss, Frost and white grounds. No background-removal mask is required.',610,15)

begin('Wallpapers with an organic edge.','Three original raster wallpapers at their native generated resolutions, with desktop and portrait compositions.')
img('wallpapers/signal-feather.png',60,160,760,428);text(60,612,'Signal Feather',23,'frost','ManropeBold');para(60,650,'1672 × 941 · Desktop. Detail and cyan live on the right so the left side stays quiet.',730,17)
img('wallpapers/signal-feather-mobile.png',907,160,253,450);text(888,634,'Signal Feather / Mobile',20,'frost','ManropeBold');para(888,671,'941 × 1672 · Portrait. Clear upper field for a clock.',320,16)

begin('Quiet alternatives for any screen.','Nightwatch carries the mascot. The vector family extends the slash with sparse geometry and crisp native-size exports.')
for x,y,f,title,desc in [(60,155,'nightwatch.png','Nightwatch','1672 × 941 PNG · engraved crow'),(660,155,'quiet-signal-4k.png','Quiet Signal','3840 × 2160 PNG + scalable SVG'),(60,453,'quiet-signal-ultrawide.png','Quiet Signal / Ultrawide','3440 × 1440 PNG + scalable SVG'),(660,453,'crowbaros-signature-4k.png','CrowbarOS Signature','3840 × 2160 PNG + scalable SVG')]:
 img('wallpapers/'+f,x,y,560,232);text(x,y+246,title,20,'frost','ManropeBold');text(x,y+276,desc,14,'muted')

begin('A desktop that gives work room.','Niri’s horizontal ribbon remains recognizable. A narrow top bar and five-app dock make the surface approachable.')
img('mockups/workspace-1600x900.png',115,149,1050,591)

begin('Focus and recognition.','Zen Mode removes the desktop chrome. The lock screen lets the crow carry the identity before sign-in.')
img('mockups/zen-1600x900.png',60,176,560,315);img('mockups/lock-1600x900.png',660,176,560,315)
text(60,524,'Zen Mode',25,'frost','ManropeBold');para(60,570,'Centered document, dimmed backdrop, no dock or top bar. The sample is shown in a user-installed writing app; it is not an extra default app.',550,18)
text(660,524,'Lock screen',25,'frost','ManropeBold');para(660,570,'A clear left-hand field keeps time and authentication legible. Nightwatch sits to the right, giving the crow a complete and dignified silhouette.',550,18)

begin('Appearance stays a choice.','A dark brand does not remove Light or Warm Paper. Studio Mode uses neutral chrome; calibration itself is outside this visual prototype.')
for x,y,f,label in [(60,142,'settings','Dark / brand default'),(660,142,'studio-neutral','Studio Mode / neutral chrome'),(60,461,'settings-light','Light / utility palette'),(660,461,'settings-paper','Warm Paper / utility palette')]:
 img(f'mockups/{f}-1600x900.png',x,y,515,290)
 # Captions sit above each proof, inside the available gap.
 text(x,y-21,label,15,'muted')

begin('Carry the identity beyond the desktop.','The boot treatment is quiet and practical. Social layouts use the same logo, type, crow and controlled accent.')
img('mockups/boot-1600x900.png',60,153,575,323);text(60,501,'Boot splash',23,'frost','ManropeBold');para(60,546,'A single mark and progress rule. Use the SVG logo at any display scale; use the micro mark for small launch surfaces.',550,18)
img('social/crowbaros-social-card-1200x630.png',685,153,535,281);text(685,457,'Social card / 1200 × 630',19,'frost','ManropeBold')
img('social/crowbaros-banner-1500x500.png',685,511,535,178);text(685,711,'Profile header / 1500 × 500',16,'muted')

begin('Ready to evaluate. Ready to build from.','Every deliverable is organized in the kit, with editable source, exact color tokens and documented asset provenance.')
rows=[('logos/','18 SVG masters + 31 PNG exports; icon sizes included.'),('mascot/','Transparent engraved crow master, 1024 × 1536.'),('wallpapers/','6 wallpapers: 3 raster originals + 3 vector designs with PNG exports.'),('mockups/','8 scene PNGs at 1600 × 900 + local HTML/CSS/JS viewer.'),('social/','1200 × 630 social card and 1500 × 500 header.'),('tokens/','CSS variables, JSON tokens and computed contrast ratios.'),('fonts/','Manrope, IBM Plex Mono, OFL texts and source information.'),('source/','Generation prompts, rendering/build scripts and verification evidence.')]
for i,(name,desc) in enumerate(rows):
 y=160+i*55;text(60,y,name,17,'ice','Plex');para(285,y,desc,915,16);line(60,y+39,1220,y+39)
para(60,623,'Open mockups/desktop.html after extracting the full ZIP. This is a review prototype, not a desktop installation. App icons are illustrative role glyphs rather than upstream application trademarks.',1130,16)
para(60,689,'Font sources: Google Fonts / Manrope and IBM Plex Mono. Artwork and identity licensing remain a project-owner decision. Domain registration and trademark clearance are not represented by this kit.',1130,14)
p.linkURL('https://github.com/google/fonts/tree/main/ofl/manrope',(60,H-721,390,H-687),relative=0)
p.linkURL('https://github.com/google/fonts/tree/main/ofl/ibmplexmono',(400,H-721,820,H-687),relative=0)
p.save();print(json.dumps({'pdf':str(out),'pages':page,'contrast':dict(ratios)},indent=2))
