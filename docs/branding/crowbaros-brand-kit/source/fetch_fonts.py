from pathlib import Path
from urllib.request import urlopen
from urllib.parse import quote

dest = Path(__file__).resolve().parents[1] / 'fonts'
files = {
    'Manrope-Variable.ttf': 'manrope/Manrope[wght].ttf',
    'Manrope-OFL.txt': 'manrope/OFL.txt',
    'IBMPlexMono-Regular.ttf': 'ibmplexmono/IBMPlexMono-Regular.ttf',
    'IBMPlexMono-OFL.txt': 'ibmplexmono/OFL.txt',
}
for name, remote in files.items():
    url = 'https://raw.githubusercontent.com/google/fonts/main/ofl/' + quote(remote)
    data = urlopen(url, timeout=30).read()
    (dest / name).write_bytes(data)
    print(name, len(data), url)
