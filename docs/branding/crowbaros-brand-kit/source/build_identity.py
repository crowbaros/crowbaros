"""Build editable SVG identity assets. Requires fontTools; no raster art manipulation."""
from pathlib import Path
import json, sys
sys.path.insert(0, '/tmp/corvid-python')
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from fontTools.pens.svgPathPen import SVGPathPen

ROOT = Path(__file__).resolve().parents[1]
palette = {'void':'#080D12','abyss':'#17212B','ice':'#49D7E8','steel':'#6D8191','frost':'#D8E5EC'}
font = instantiateVariableFont(TTFont(ROOT/'fonts/Manrope-Variable.ttf'), {'wght':700}, inplace=False)
for record in font['name'].names:
    if record.nameID == 6: record.string = 'CrowbarManrope-Bold'.encode(record.getEncoding())
font.save('/tmp/Crowbar-Manrope-Bold.ttf')
regular = instantiateVariableFont(TTFont(ROOT/'fonts/Manrope-Variable.ttf'), {'wght':400}, inplace=False)
for record in regular['name'].names:
    if record.nameID == 6: record.string = 'CrowbarManrope-Regular'.encode(record.getEncoding())
regular.save('/tmp/Crowbar-Manrope-Regular.ttf')
glyphs = font.getGlyphSet(); cmap=font.getBestCmap(); upem=font['head'].unitsPerEm

def word(x,y,size,color):
    paths=[]; cursor=0
    for char in 'CrowbarOS':
        name=cmap[ord(char)]; pen=SVGPathPen(glyphs); glyphs[name].draw(pen)
        paths.append(f'<path transform="translate({cursor},0)" d="{pen.getCommands()}"/>')
        cursor += glyphs[name].width - upem*.025
    return f'<g fill="{color}" transform="translate({x},{y}) scale({size/upem},-{size/upem})">'+''.join(paths)+'</g>'

def mark(fg,accent, micro=False):
    eye='' if micro else ' M127 54 Q132 53 136 57 Q131 62 127 54 Z'
    return f'<path fill="{accent}" d="M14 144H43L115 16H86Z"/><path fill="{fg}" fill-rule="evenodd" d="M119 38C137 40 148 47 156 57C179 64 194 76 200 94C180 86 162 85 147 89C128 94 116 105 108 121C106 101 110 84 122 73C130 66 140 63 151 63C141 52 128 49 113 49Z{eye}"/>'

def svg(name,w,h,body):
    path=ROOT/'logos/svg'/f'{name}.svg'
    path.write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img"><title>CrowbarOS — {name}</title>{body}</svg>')

variants={'primary':('#D8E5EC','#49D7E8'),'dark':('#080D12','#080D12'),'frost':('#D8E5EC','#D8E5EC'),'ice':('#49D7E8','#49D7E8')}
for label,(fg,acc) in variants.items():
    svg(f'crowbaros-mark-{label}',256,256,f'<g transform="translate(20,48)">{mark(fg,acc)}</g>')
    svg(f'crowbaros-horizontal-{label}',850,180,f'<g transform="translate(0,10)">{mark(fg,acc)}</g>'+word(233,125,108,fg))
    svg(f'crowbaros-stacked-{label}',680,420,f'<g transform="translate(234,20)">{mark(fg,acc)}</g>'+word(68,346,102,fg))
    svg(f'crowbaros-wordmark-{label}',640,160,word(20,115,110,fg))
svg('crowbaros-mark-micro',256,256,f'<g transform="translate(20,48)">{mark("#D8E5EC","#49D7E8",True)}</g>')
svg('crowbaros-app-icon',256,256,'<rect width="256" height="256" rx="48" fill="#080D12"/>'+f'<g transform="translate(20,48)">{mark("#D8E5EC","#49D7E8")}</g>')

for name,w,h,branded in [('quiet-signal-4k',3840,2160,False),('quiet-signal-ultrawide',3440,1440,False),('crowbaros-signature-4k',3840,2160,True)]:
    # Exact large vector planes extend the logo slash; this wallpaper has no raster dependency.
    shapes=f'<rect width="{w}" height="{h}" fill="#080D12"/><path d="M{w*.63} {h}H{w*.79}L{w*1.12} 0H{w*.96}Z" fill="#101A23"/><path d="M{w*.75} {h}H{w*.84}L{w*1.17} 0H{w*1.08}Z" fill="#17212B"/><path d="M{w*.777} {h}H{w*.783}L{w*.977} {h*.41}H{w*.971}Z" fill="#49D7E8"/>'
    if branded: shapes+=f'<g transform="translate({w*.09},{h*.42}) scale(2.5)">{mark("#D8E5EC","#49D7E8")}</g>'+word(w*.09+570,h*.42+300,230,'#D8E5EC')
    (ROOT/'wallpapers'/f'{name}.svg').write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{shapes}</svg>')

tokens={'brand':'CrowbarOS','version':'1.0','palette':palette,'semantic':{'background':'{void}','surface':'{abyss}','text':'{frost}','accent':'{ice}','decorative':'{steel}','secondaryText':'#A8BAC7','neutralStudioBackground':'#111111','neutralStudioSurface':'#212121','neutralStudioText':'#E5E5E5'},'typography':{'ui':'Manrope','wordmark':'Manrope 700 with custom spacing; use outlined master','code':'IBM Plex Mono'},'radii':{'window':16,'control':10},'spacing':[4,8,12,16,24,32,48,64]}
(ROOT/'tokens/crowbaros.tokens.json').write_text(json.dumps(tokens,indent=2)+'\n')
css=':root {\n'+''.join(f'  --crowbar-{k}: {v};\n' for k,v in palette.items())+'  --crowbar-muted: #A8BAC7;\n  --crowbar-ui: "Manrope", sans-serif;\n  --crowbar-code: "IBM Plex Mono", monospace;\n  --crowbar-radius-window: 16px;\n  --crowbar-radius-control: 10px;\n}\n'
(ROOT/'tokens/crowbaros.css').write_text(css)
print('Built 18 SVG logos, 3 SVG wallpaper masters, CSS and JSON tokens.')
