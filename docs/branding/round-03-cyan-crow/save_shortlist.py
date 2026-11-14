from pathlib import Path
from datetime import datetime
from zoneinfo import ZoneInfo
import json

root=Path(__file__).resolve().parent
checks=json.loads((root.parent/'naming-domain-checks.json').read_text())
by_name={r['name']:r for r in checks['results']}
names=[
 ('CorvidOS','Best overall fit: the crow family, intelligence and a concise technical sound.'),
 ('GloamOS','Twilight and a dark atmosphere; softer and more evocative.'),
 ('QuothOS','A literary raven association; distinctive, though pronunciation is less obvious.'),
 ('CowlOS','Short, dark and enigmatic; the hood-like silhouette can echo a crow\'s head.'),
 ('SootOS','Compact and tactile; a matte-black, industrial personality.'),
 ('NacreOS','Mother-of-pearl suggests a subtle iridescent cyan sheen; pronunciation is less obvious.'),
 ('RookOS','Very concise and directly tied to a corvid; the requested .com is already registered.'),
 ('SpectreOS','A clear stealth association; the requested .com is already registered.'),
 ('SableOS','Elegant black-color association; the requested .com is already registered.'),
 ('VesperOS','Evening atmosphere; the requested .com is already registered.')]
shortlist=[dict(by_name[name],reason=reason) for name,reason in names]
last=max(datetime.fromisoformat(r['checked_at_utc']) for r in checks['results'])
output={
 'direction':{'palette_from':'Nullkeel','mascot_from':'Rookslash','naming_format':'One word followed by OS','selected_name':None},
 'palette':[['Void','#080D12'],['Abyss','#17212B'],['Ice','#49D7E8'],['Steel','#6D8191'],['Frost','#D8E5EC']],
 'recommendation':['CorvidOS','GloamOS','QuothOS'],
 'checked_at_arizona':last.astimezone(ZoneInfo('America/Phoenix')).isoformat(),
 'checked_count':len(checks['results']),
 'unregistered_count':sum(r['status']=='unregistered' for r in checks['results']),
 'availability_note':'Unregistered means the authoritative .com RDAP endpoint returned HTTP 404 at check time; registrar checkout is the final availability and price confirmation. Registered does not establish whether an owner may sell. No domains were purchased or reserved. This is not trademark clearance.',
 'shortlist':shortlist,
 'visual_study':{'file':'cyan-crow-name-studies.png','generation':'built-in image_gen','prompt':'generation-prompt.txt','names_are_alternatives':True}}
(root/'shortlist.json').write_text(json.dumps(output,indent=2)+'\n')
print('Saved shortlist:',root/'shortlist.json')
print('Checked:',output['checked_at_arizona'])
print('Domains:',output['checked_count'],'Unregistered:',output['unregistered_count'])
