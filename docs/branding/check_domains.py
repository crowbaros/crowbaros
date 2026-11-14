"""Read-only .com registry checks; saves time-stamped public registration evidence."""
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
import json
import urllib.error
import urllib.request

NAMES = ['Rook', 'Spectre', 'Corvid', 'Sable', 'Vesper', 'Umber', 'Umbra', 'Talon', 'Quoth', 'Gloam', 'Ebon', 'Noctis', 'Vanta', 'Corvus', 'Nyx', 'Fathom', 'Cowl', 'Quill', 'Raven', 'Onyx', 'Eyrie', 'Pinion', 'Plume', 'Nacre', 'Obscura', 'Noir', 'Nox', 'Murk', 'Soot', 'Dusk']
DEST = Path(__file__).resolve().parent / 'naming-domain-checks.json'

def check(name):
    domain = name.lower() + 'os.com'
    url = 'https://rdap.verisign.com/com/v1/domain/' + domain
    result = {'name':name+'OS','domain':domain,'source':url,'checked_at_utc':datetime.now(timezone.utc).isoformat()}
    req = urllib.request.Request(url,headers={'User-Agent':'Mozilla/5.0','Accept':'application/rdap+json, application/json'})
    try:
        with urllib.request.urlopen(req,timeout=20) as response:
            data=json.load(response)
            result.update(http_status=response.status,status='registered',registry_name=data.get('ldhName'),events=data.get('events'),status_flags=data.get('status'))
    except urllib.error.HTTPError as e:
        result.update(http_status=e.code,status='unregistered' if e.code==404 else 'unknown')
        try: result['registry_response']=json.loads(e.read().decode('utf-8'))
        except (ValueError,UnicodeDecodeError): result['error']=str(e)
    except Exception as e:
        result.update(status='unknown',error=str(e))
    return result

with ThreadPoolExecutor(max_workers=4) as pool:
    results=list(pool.map(check,NAMES))
DEST.write_text(json.dumps({'method':'Verisign authoritative .com RDAP. HTTP 200 means registered; HTTP 404 means no registration record at query time. This is not a purchase or registrar reservation.','results':results},indent=2)+'\n')
for result in results:
    print(result['name'], result['domain'], result['status'], result.get('http_status',result.get('error')))
print('Saved:',DEST)
