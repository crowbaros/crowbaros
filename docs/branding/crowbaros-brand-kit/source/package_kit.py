"""Validate and package the final CrowbarOS asset set without changing image pixels."""
from pathlib import Path
from PIL import Image
from pypdf import PdfReader
from hashlib import sha256
import json, zipfile, re, xml.etree.ElementTree as ET

root=Path(__file__).resolve().parents[1]
required=['README.txt','DESIGN.md','.impeccable/design.json','guide/CrowbarOS-brand-guide.pdf','mockups/desktop.html','fonts/Manrope-OFL.txt','fonts/IBMPlexMono-OFL.txt','source/image-prompts.json','source/provenance.json','source/finish-review.txt']
for f in required:assert (root/f).is_file(),f
json.loads((root/'.impeccable/design.json').read_text())
logos=list((root/'logos/svg').glob('*.svg'));pngs=list((root/'logos/png').glob('*.png'));walls=list((root/'wallpapers').glob('*.png'));screens=list((root/'mockups').glob('*.png'))
assert len(logos)==18 and len(pngs)==31 and len(walls)==6 and len(screens)==8
for f in logos:
 text=f.read_text();ET.fromstring(text)
 assert '<text' not in text and '<image' not in text, f
for f in screens:assert Image.open(f).size==(1600,900),f
expected={'signal-feather.png':(1672,941),'nightwatch.png':(1672,941),'signal-feather-mobile.png':(941,1672),'quiet-signal-4k.png':(3840,2160),'quiet-signal-ultrawide.png':(3440,1440),'crowbaros-signature-4k.png':(3840,2160)}
for f,size in expected.items():assert Image.open(root/'wallpapers'/f).size==size,f
crow=Image.open(root/'mascot/crow-master.png');assert crow.mode=='RGBA' and crow.size==(1024,1536) and crow.getextrema()[3][0]==0
for f in pngs:
 im=Image.open(f);assert im.mode=='RGBA' and im.getextrema()[3][0]==0,f
pdf=PdfReader(root/'guide/CrowbarOS-brand-guide.pdf');assert len(pdf.pages)==14
for page in pdf.pages:assert len(page.extract_text().strip())>50
tokens=json.loads((root/'tokens/crowbaros.tokens.json').read_text());assert list(tokens['palette'].values())==['#080D12','#17212B','#49D7E8','#6D8191','#D8E5EC']
html=(root/'mockups/desktop.html').read_text()
for relative in re.findall(r'(?:src|href)="([^"]+)"',html):assert (root/'mockups'/relative).resolve().is_file(),relative
report={'status':'pass','date':'2026-11-14','svg_logos':18,'png_logo_exports':31,'wallpapers':6,'wallpaper_svg_masters':3,'mockup_pngs':8,'pdf_pages':14,'pdf_visual_review':'All 14 renamed guide pages rendered and inspected; original layout preserved.','mascot_alpha':'Native RGBA, zero-alpha background, verified on four grounds.','viewer_review':'Renamed desktop/mobile viewer and all eight scenes rendered; fit and theme interactions verified. See finish-review.txt.','hook_findings':'Brand geometry and typography preserved. See page-scoped configuration for the inherited template-only false-positive ignore.','scope':'Design assets and local review prototype; no OS implementation, installation or domain registration.'}
(root/'source/verification.json').write_text(json.dumps(report,indent=2)+'\n')
def files():return sorted(f for f in root.rglob('*') if f.is_file() and '__pycache__' not in f.parts and f.name not in ['hook.cache.json','asset-manifest.json'])
inventory=[]
for f in files():
 record={'path':str(f.relative_to(root)),'bytes':f.stat().st_size,'sha256':sha256(f.read_bytes()).hexdigest()}
 if f.suffix=='.png':
  with Image.open(f) as im:record.update(width=im.width,height=im.height,mode=im.mode)
 inventory.append(record)
(root/'asset-manifest.json').write_text(json.dumps({'brand':'CrowbarOS','version':'1.0','date':'2026-11-14','files':inventory},indent=2)+'\n')
archive=root.parent/'CrowbarOS-brand-kit.zip'
with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED,compresslevel=6) as z:
 for f in files()+[root/'asset-manifest.json']:z.write(f,root.name+'/'+str(f.relative_to(root)))
with zipfile.ZipFile(archive) as z:
 assert z.testzip() is None
 assert len(z.namelist())==len(inventory)+1
print(json.dumps({'archive':str(archive),'size_mib':round(archive.stat().st_size/1024**2,1),'files':len(inventory)+1,'checks':report},indent=2))
