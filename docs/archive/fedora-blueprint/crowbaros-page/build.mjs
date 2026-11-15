import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { marked } from '/home/jasona/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/marked/lib/marked.esm.js';
import { createRequire } from 'node:module';
const sharp=createRequire(import.meta.url)('/home/jasona/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/sharp');
const here=path.dirname(fileURLToPath(import.meta.url)),docs=path.dirname(here);
const source=fs.readFileSync(path.join(docs,'community_foss_distro_plan.md'),'utf8');
let reference=marked.parse(source,{gfm:true});
reference=reference.replace(/<h([1-6])>([\s\S]*?)<\/h\1>/g,(_,level,label)=>`<h${Math.min(+level+2,6)}>${label}</h${Math.min(+level+2,6)}>`);
reference=reference.replace(/<table>/g,'<div class="table-scroll"><table>').replace(/<\/table>/g,'</table></div>');
reference=reference.replace(/<pre><code class="language-mermaid">([\s\S]*?)<\/code><\/pre>/g,'<details class="diagram-source"><summary>View the original diagram notation</summary><pre><code>$1</code></pre></details>');
let refCount=0;
reference=reference.split(/(?=<h[34]>)/).map(chunk=>{const title=(chunk.match(/<h[34]>(.*?)<\/h[34]>/)||[])[1]?.replace(/<[^>]*>/g,'')||'Source introduction';return `<div class="reference-block" id="reference-${refCount++}" data-search="${title.replaceAll('"','&quot;')}">${chunk}</div>`;}).join('');
const allLines=source.split('\n');
function tableAfter(header){const start=allLines.findIndex(s=>s===header);const rows=[];for(let i=start+2;i<allLines.length&&allLines[i].startsWith('|');i++)rows.push(allLines[i].slice(1,-1).split('|').map(s=>s.trim()));return rows;}
const systems=tableAfter('| Component | Package(s) | License | Role |');
const packages=systems.map(([name,pkg,license,role])=>`<tr data-package><th scope="row">${marked.parseInline(name)}</th><td>${marked.parseInline(pkg)}</td><td>${marked.parseInline(license)}</td><td>${marked.parseInline(role)}</td></tr>`).join('');
const core=tableAfter('| App Slot | Package | Delivery | Why It Fits |');
const appLabels=['Zen Browser','Files','Ghostty','Studio Settings','Bazaar'];
const appRoles=['Browse','Organize','Build','Personalize','Discover'];
const apps=core.map(([slot,pkg,delivery,role],i)=>`<details class="app-row" ${i===0?'open':''} data-search="${appLabels[i]} ${appRoles[i]}" id="app-${i}"><summary><span class="app-name">${appLabels[i]}</span><span class="app-role">${appRoles[i]}</span><svg class="icon chevron" viewBox="0 0 24 24" aria-hidden="true"><path d="m8 5 7 7-7 7"/></svg></summary><div class="app-detail"><p>${marked.parseInline(role)}</p><dl><div><dt>Package</dt><dd>${marked.parseInline(pkg)}</dd></div><div><dt>Delivery</dt><dd>${marked.parseInline(delivery)}</dd></div></dl></div></details>`).join('');
let html=fs.readFileSync(path.join(here,'page.template.html'),'utf8');
const changes={'{{CSS}}':fs.readFileSync(path.join(here,'page.css'),'utf8'),'{{JS}}':fs.readFileSync(path.join(here,'page.js'),'utf8'),'{{REFERENCE}}':reference,'{{PACKAGE_ROWS}}':packages,'{{APP_ROWS}}':apps,'{{SOURCE_JSON}}':JSON.stringify(source).replaceAll('<','\\u003c')};
for(const [token,value]of Object.entries(changes))html=html.replace(token,()=>value);
// Embed only previews and small reference files. Keep native image dimensions.
const mime={svg:'image/svg+xml',ttf:'font/ttf',md:'text/markdown',json:'application/json',css:'text/css',txt:'text/plain'};
const prefix='branding/crowbaros-brand-kit/',assets={},initial=new Set(),visuals=[];
const files=new Set([...html.matchAll(/(?:src|href)="(branding\/[^"]+)"|url\('(branding\/[^']+)'\)/g)].map(m=>m[1]||m[2]));
for(const name of ['workspace','zen','settings','settings-light','settings-paper','studio-neutral','lock','boot'])files.add(prefix+'mockups/'+name+'-1600x900.png');
for(const name of ['signal-feather','nightwatch','quiet-signal-4k','quiet-signal-ultrawide','crowbaros-signature-4k','signal-feather-mobile'])files.add(prefix+'wallpapers/'+name+'.png');
for(const name of ['primary','frost','dark','ice'])files.add(prefix+'logos/svg/crowbaros-horizontal-'+name+'.svg');
for(const name of ['Manrope-OFL.txt','IBMPlexMono-OFL.txt'])files.add(prefix+'fonts/'+name);
for(const file of files){
  if(file.endsWith('.zip')||file.endsWith('.pdf')||file.endsWith('/desktop.html')||file.endsWith('/CrowbarOS-brand-overview.png'))continue;
  let bytes=fs.readFileSync(path.join(docs,file)),type=mime[path.extname(file).slice(1)];
  if(file.endsWith('.png')){const metadata=await sharp(bytes).metadata();bytes=await sharp(bytes).webp({quality:file.includes('/mockups/')?92:88,alphaQuality:100,effort:5}).toBuffer();type='image/webp';visuals.push({source:file,encoding:'WebP',quality:file.includes('/mockups/')?92:88,width:metadata.width,height:metadata.height,bytes:bytes.length});}
  if(!type)throw Error('Unsupported asset: '+file);
  assets[file]={url:`data:${type};base64,${bytes.toString('base64')}`};
}
html=html.replace(/src="(branding\/[^"]+)"/g,(_,file)=>{initial.add(file);return `data-embedded-src="${file}" src="${assets[file].url}"`;});
html=html.replace(/url\('(branding\/[^']+)'\)/g,(_,file)=>{initial.add(file);return `url('${assets[file].url}')`;});
html=html.replace(/(<link rel="icon" href=")(branding\/[^"]+)(")/,(_,before,file,after)=>{initial.add(file);return before+assets[file].url+after;});
html=html.replace(/<a href="branding\/[^\"]+\.(?:zip|pdf)">([^<]+)<\/a>/g,'<span>$1 (separate brand-kit resource)</span>');
html=html.replace(/<a\b([^>]*?)href="(branding\/[^\"]+)"([^>]*)>/g,(_,before,file,after)=>{
  if(file.endsWith('/mockups/desktop.html'))return `<a${before}href="#desktop"${after.replace(/\s+(target|rel)="[^"]*"/g,'')}>`;
  if(file.endsWith('/CrowbarOS-brand-overview.png'))return `<a${before}href="#identity"${after}>`;
  if(!assets[file])throw Error('Unbundled download: '+file);
  return `<a${before}href="#resources" data-embedded-file="${file}"${after.replace(/\s+(target|rel)="[^"]*"/g,'')}>`;
});
const embedded=fs.readFileSync(path.join(here,'embedded.js'),'utf8');
const secondary=Object.fromEntries(Object.entries(assets).filter(([file])=>!initial.has(file)));
html=html.replace('<script>'+changes['{{JS}}'],()=>`<script type="application/json" id="embedded-assets">${JSON.stringify(secondary)}</script><script>${embedded}</script><script>`+changes['{{JS}}']);
if(/\{\{[A-Z_]+\}\}/.test(html))throw Error('Unexpanded template token');
fs.writeFileSync(path.join(docs,'crowbaros.html'),html);
fs.writeFileSync(path.join(here,'content-coverage.json'),JSON.stringify({source:'docs/community_foss_distro_plan.md',sourceBytes:Buffer.byteLength(source),completeSourceEmbedded:true,renderedReferenceBlocks:refCount,systemPackageRows:systems.length,coreAppRows:core.length,sourceSha256:(await import('node:crypto')).createHash('sha256').update(source).digest('hex')},null,2));
fs.writeFileSync(path.join(here,'visual-provenance.json'),JSON.stringify({sourceProvenance:'../branding/crowbaros-brand-kit/source/provenance.json',operation:'Format compression only; native dimensions and transparent alpha retained. No new generated artwork.',visuals},null,2));
console.log(`Built compact self-contained docs/crowbaros.html: ${(Buffer.byteLength(html)/1048576).toFixed(2)} MiB, ${refCount} reference blocks, ${systems.length} package groups, ${core.length} core apps; no ZIP or PDF embedded.`);
