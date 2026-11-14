"""Rebuild the review booklet from the original concept boards and manifest."""
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
OUT = ROOT / 'community-distro-brand-concepts.pdf'
DATA = json.loads((ROOT / 'concepts.json').read_text())['concepts']
for name, filename in [('Sans','NotoSans-Regular.ttf'),('SansBold','NotoSans-Bold.ttf'),('Serif','NotoSerif-Regular.ttf')]:
    pdfmetrics.registerFont(TTFont(name, '/usr/share/fonts/noto/' + filename))
W = H = 1080
INK, MUTED, PAPER, RULE = map(HexColor, ['#282C2B','#626966','#FAF8F3','#D9DDD6'])
c = canvas.Canvas(str(OUT), pagesize=(W,H), pageCompression=1)
c.setTitle('Warm Human Craft - Ten Distro Brand Directions')
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
    text('WARM HUMAN CRAFT',48,1032,11,'SansBold',MUTED)
    c.setFont('Sans',11); c.drawRightString(1032,1032,label)
    c.setStrokeColor(RULE); c.setLineWidth(.6); c.line(48,1013,1032,1013)
    c.line(48,48,1032,48)
    text('COMMUNITY FOSS DESKTOP / BRAND EXPLORATION / 13 NOVEMBER 2026',48,28,9,color=MUTED)
    c.setFont('Sans',9); c.drawRightString(1032,28,f'{page:02d} / 12')

base(1,'ROUND 01 / TEN DIRECTIONS')
text('Ten ways to feel at home.',48,940,46,'Serif')
para('A calm, welcoming studio for everyday computing. These directions translate the plan\'s warm typography, creative freedom and lean desktop into names, mascots, palettes and wallpaper concepts.',48,905,900,18,28)
text('NAME',48,800,11,'SansBold',MUTED)
text('CREATIVE TERRITORY',280,800,11,'SansBold',MUTED)
text('PALETTE',604,800,11,'SansBold',MUTED)
text('MASCOT',835,800,11,'SansBold',MUTED)
animals=['Harvest mouse','Leaf-quilled hedgehog','Red panda','Paper crane','Mountain hare','Tern','Ceramic gecko','Paper moth','Pond frog','Pigment lark']
for i,d in enumerate(DATA):
    y=760-i*54
    text(f'{i+1:02d}',48,y,12,color=MUTED)
    text(d['name'],82,y,18,'SansBold')
    text(d['territory'],280,y+1,14)
    for j,(_,hx) in enumerate(d['palette']):
        c.setFillColor(HexColor(hx));c.rect(604+j*47,y-4,39,24,fill=1,stroke=0)
    text(animals[i],835,y+1,13)
    c.setStrokeColor(RULE);c.line(48,y-20,1032,y-20)
text('START WITH THESE THREE',48,208,11,'SansBold',MUTED)
for x,title,body in [(48,'Selveday','Closest to the warm craft brief.'),(390,'Kilnvale','Most memorable maker mascot.'),(732,'Ochrelark','Strongest visual-arts identity.')]:
    text(title,x,174,23,'Serif')
    para(body,x,153,300,14,21)
para('Each direction includes a raster identity board: wordmark, symbol, mascot, wallpaper preview, palette and light/dark mark studies. The desktop previews communicate visual tone; their dock icons are illustrative.',48,105,984,11,16,MUTED)
c.showPage()

for i,d in enumerate(DATA):
    base(i+2, f'{i+1:02d} / {d["name"].upper()} / {d["territory"].upper()}')
    c.bookmarkPage(d['id']);c.addOutlineEntry(d['name'],d['id'],0,False)
    c.drawImage(str(ROOT/'assets'/f'{d["id"]}.png'),48,345,width=984,height=656,preserveAspectRatio=True,mask='auto')
    for x,label in [(48,'THE NAME'),(390,'THE MASCOT & FIT'),(732,'THE TRADEOFF')]:text(label,x,309,10,'SansBold',MUTED)
    y=para(d['why'],48,289,300,13,19)
    text(d['pronunciation'],48,y-25,11,'SansBold',MUTED)
    y=para(d['mascot']+'.',390,289,300,13,19)
    para(d['strength'],390,y-12,300,13,19)
    para(d['tradeoff'],732,289,300,13,19)
    for j,(name,hx) in enumerate(d['palette']):
        x=48+j*252
        c.setFillColor(HexColor(hx));c.rect(x,98,228,26,fill=1,stroke=0)
        text(name,x,79,11,'SansBold')
        c.setFont('Sans',11);c.setFillColor(MUTED);c.drawRightString(x+228,79,hx)
    c.showPage()

base(12,'REVIEW NOTES / NEXT ROUND')
text('Choose the feeling first.',48,944,43,'Serif')
para('My recommendation is to carry Selveday, Kilnvale and Ochrelark into a second round. Together they test three meaningfully different promises: a shared workshop, tangible craft, and an independent creative studio.',48,906,930,18,28)

text('WHAT THE PLAN SAYS TO THE BRAND',48,785,11,'SansBold',MUTED)
items=[
 ('Lead with everyday ease.','Open with a calm desktop and room for your work. Keep Fedora, bootc, Niri and package architecture in technical documentation.'),
 ('Use precise community language.','Community-built fits the proposed contributor model. Community-owned or fully democratic would misrepresent the founder\'s reserved majority control.'),
 ('Be clear about the freedom promise.','Say FOSS userland with practical hardware support. The plan itself includes a separate hardware-support policy; avoid compressing that into an unqualified purity claim.'),
 ('Let the artwork carry the personality.','Mascots belong in wallpapers, welcome art and community materials. Preserve the plan\'s calm, AI-neutral daily desktop.'),
 ('Keep color-critical work neutral.','These palettes are brand starting points. Studio Mode needs neutral chrome; semantic status colors and accessible interaction tokens still need separate design and testing.')]
y=749
for title,body in items:
    text(title,48,y,17,'SansBold')
    y=para(body,48,y-15,460,14,21)-30

text('QUESTIONS FOR THE NEXT ROUND',588,785,11,'SansBold',MUTED)
for j,s in enumerate(['Can you comfortably say and spell the name after hearing it once?', 'Does the wallpaper feel calm after a full working day?', 'Would a first-time Linux user find the mascot welcoming?', 'Does the mark still work at boot-icon size and in one color?']):
    para(f'{j+1}.  {s}',588,750-j*90,430,16,24)
text('EXPLORATION STATUS',588,361,11,'SansBold',MUTED)
para('These are generated raster concepts, not final vector identities. Exact palette values are specified in the text and manifest; generated image swatches may vary. Names received a limited web screen, not trademark, domain, handle or linguistic clearance.',588,338,430,13,21,MUTED)
para('Source brief: docs/community_foss_distro_plan.md. Full prompts, naming rationale, palettes and tradeoffs are preserved in concepts.json. Boards were generated using the built-in image_gen tool.',48,102,960,10,15,MUTED)
c.save()
print(OUT)
