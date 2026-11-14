"""Rebuild the dark identity review booklet from the boards and manifest."""
from pathlib import Path
from xml.sax.saxutils import escape
import json
from reportlab.pdfgen import canvas
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.lib.colors import HexColor
from reportlab.lib.styles import ParagraphStyle
from reportlab.platypus import Paragraph

ROOT = Path(__file__).resolve().parent
OUT = ROOT / 'dark-hacker-brand-concepts.pdf'
DATA = json.loads((ROOT / 'concepts.json').read_text())['concepts']
for name, filename in [('Sans','NotoSans-Regular.ttf'),('SansBold','NotoSans-Bold.ttf'),('Serif','NotoSerif-Regular.ttf')]:
    pdfmetrics.registerFont(TTFont(name, '/usr/share/fonts/noto/' + filename))
W = H = 1080
INK, MUTED, PAPER, RULE = map(HexColor, ['#E7ECEF','#A0ADB6','#101418','#343D45'])
c = canvas.Canvas(str(OUT), pagesize=(W,H), pageCompression=1)
c.setTitle('After Dark - Ten Hacker Distro Brand Directions')
c.setAuthor('Community FOSS Distro - Brand Exploration')

def text(s,x,y,size=14,font='Sans',color=INK):
    c.setFont(font,size); c.setFillColor(color); c.drawString(x,y,s)

def para(s,x,top,width=300,size=14,leading=21,color=INK,font='Sans'):
    p = Paragraph(escape(s), ParagraphStyle('p',fontName=font,fontSize=size,leading=leading,textColor=color))
    _, h = p.wrap(width,900)
    p.drawOn(c,x,top-h)
    return top-h

def base(page, label):
    c.setFillColor(PAPER); c.rect(0,0,W,H,fill=1,stroke=0)
    text('AFTER DARK / UNDER YOUR CONTROL',48,1032,11,'SansBold',MUTED)
    c.setFont('Sans',11);c.drawRightString(1032,1032,label)
    c.setStrokeColor(RULE);c.setLineWidth(.6);c.line(48,1013,1032,1013)
    c.line(48,48,1032,48)
    text('COMMUNITY FOSS DESKTOP / BRAND EXPLORATION / 13 NOVEMBER 2026',48,28,9,color=MUTED)
    c.setFont('Sans',9);c.drawRightString(1032,28,f'{page:02d} / 12')

def luminance(hx):
    rgb=[int(hx[i:i+2],16)/255 for i in (1,3,5)]
    linear=[v/12.92 if v<=.04045 else ((v+.055)/1.055)**2.4 for v in rgb]
    return sum(a*b for a,b in zip(linear,[.2126,.7152,.0722]))

def contrast(a,b):
    hi,lo=sorted((luminance(a),luminance(b)),reverse=True)
    return (hi+.05)/(lo+.05)

base(1,'ROUND 02 / TEN NEW DIRECTIONS')
text('Dark by design.',48,940,50,'SansBold')
para('Ten distinct identities for technically fluent Linux users, tinkerers and hackers. New names, mascots and palettes explore stealth, terminal culture, industrial craft, fluid systems and architectural strength.',48,903,950,18,28)
text('NAME',48,800,11,'SansBold',MUTED)
text('CREATIVE TERRITORY',280,800,11,'SansBold',MUTED)
text('PALETTE',592,800,11,'SansBold',MUTED)
text('MASCOT',835,800,11,'SansBold',MUTED)
animals=['Cuttlefish','Badger','Salamander','Faceted lynx','Engraved raven','Stag beetle','Emerald serpent','Basalt bison','Spectral dragonfly','Horseshoe crab']
for i,d in enumerate(DATA):
    y=760-i*54
    text(f'{i+1:02d}',48,y,12,color=MUTED)
    text(d['name'],82,y,18,'SansBold')
    text(d['territory'],280,y+1,14)
    for j,(_,hx) in enumerate(d['palette']):
        c.setFillColor(HexColor(hx));c.rect(592+j*42,y-4,35,24,fill=1,stroke=0)
    text(animals[i],835,y+1,13)
    c.setStrokeColor(RULE);c.line(48,y-20,1032,y-20)
text('START WITH THESE THREE',48,208,11,'SansBold',MUTED)
for x,title,body in [(48,'Nullkeel','Sleek, controlled and quietly technical.'),(390,'Hexburrow','The clearest classic hacker identity.'),(732,'Rookslash','The most distinctive editorial edge.')]:
    text(title,x,174,23,'SansBold')
    para(body,x,153,300,14,21)
para('Each direction includes a wordmark, symbol, original mascot, dark wallpaper preview, five-color palette and monochrome mark studies. Desktop frames communicate visual tone; the dock symbols are illustrative.',48,105,984,11,16,MUTED)
c.showPage()

ratios={}
for i,d in enumerate(DATA):
    base(i+2, f'{i+1:02d} / {d["name"].upper()} / {d["territory"].upper()}')
    c.bookmarkPage(d['id']);c.addOutlineEntry(d['name'],d['id'],0,False)
    c.drawImage(str(ROOT/'assets'/f'{d["id"]}.png'),48,345,width=984,height=656,preserveAspectRatio=True,mask='auto')
    for x,label in [(48,'THE NAME'),(390,'THE MASCOT & FIT'),(732,'THE TRADEOFF')]:text(label,x,309,10,'SansBold',MUTED)
    y=para(d['why'],48,289,300,13,19)
    text(d['pronunciation'],48,y-25,11,'SansBold',MUTED)
    y=para(d['mascot']+'.',390,289,300,13,19)
    para(d['strength'],390,y-12,300,13,19)
    y=para(d['tradeoff'],732,289,300,13,19)
    foreground=d['palette'][-1][1]
    ratios[d['name']]=[contrast(foreground,d['palette'][j][1]) for j in (0,1)]
    r1,r2=ratios[d['name']]
    para(f'Pale text contrast on base / surface: {r1:.1f}:1 / {r2:.1f}:1.',732,y-15,300,10,15,MUTED)
    for j,(name,hx) in enumerate(d['palette']):
        x=48+j*200
        c.setFillColor(HexColor(hx));c.rect(x,98,184,26,fill=1,stroke=0)
        text(name,x,79,11,'SansBold')
        c.setFont('Sans',11);c.setFillColor(MUTED);c.drawRightString(x+184,79,hx)
    c.showPage()

base(12,'REVIEW NOTES / NEXT ROUND')
text('Choose the kind of dark.',48,944,43,'SansBold')
para('Nullkeel is the strongest restrained system brand. Hexburrow makes the hacker audience immediately legible. Rookslash has the most distinctive personality and preserves a link to the original editorial brief. These are design judgments, not user-test results.',48,906,950,18,28)

text('HOW TO KEEP THE DARKNESS USEFUL',48,785,11,'SansBold',MUTED)
items=[
 ('Separate dark surfaces.','Use distinct base and raised-surface values so panes, menus and window edges remain readable. Do not rely entirely on a neon outline.'),
 ('Give the accent a job.','Use the strongest hue for selection, focus and key actions. Keep most long-reading surfaces quiet; a vivid identity can support a restrained daily UI.'),
 ('Let the mascot scale down.','These illustrations establish character and material. A final identity needs a simplified silhouette that survives a small icon, a one-color sticker and a boot screen.'),
 ('Preserve the underlying freedoms.','The hacker-facing art does not require changing the plan\'s GUI accessibility, lean defaults, FOSS userland or AI neutrality.'),
 ('Protect color-critical work.','Studio Mode should still use neutral chrome. Bright red, green and spectral accents need their own semantic rules so branding does not obscure errors or status.')]
y=749
for title,body in items:
    text(title,48,y,17,'SansBold')
    y=para(body,48,y-15,460,14,21)-30

text('QUESTIONS FOR THE NEXT ROUND',588,785,11,'SansBold',MUTED)
for j,s in enumerate(['Which would you want on a laptop lid and in a boot menu?', 'Which wallpaper stays comfortable during a long coding session?', 'Which name is easiest to say, spell and remember?', 'Does the brand feel like a credible system you could make your own?']):
    para(f'{j+1}.  {s}',588,750-j*90,430,16,24)
text('EXPLORATION STATUS',588,361,11,'SansBold',MUTED)
para('Generated raster concepts, not final vector identities. The text and manifest specify exact palette values; generated swatches may vary. Names received a limited web screen, not trademark, domain, handle or linguistic clearance.',588,338,430,13,21,MUTED)
minimum=min(x for rs in ratios.values() for x in rs)
para(f'Minimum pale-text contrast across the specified dark surfaces: {minimum:.2f}:1. This checks those color pairs only, not the complete UI or generated artwork.',588,201,430,11,17,MUTED)
para('Source brief: docs/community_foss_distro_plan.md, plus the request for ten wholly new dark hacker directions. Full prompts and design notes: concepts.json. Artwork generated using the built-in image_gen tool.',48,102,960,10,15,MUTED)
c.save()
(ROOT/'palette-contrast.json').write_text(json.dumps({'method':'sRGB relative luminance contrast ratio','foreground':'last palette color','backgrounds':['first palette color','second palette color'],'ratios':ratios},indent=2)+'\n')
print(OUT)
print(f'Minimum specified pale-text contrast: {minimum:.4f}:1')
