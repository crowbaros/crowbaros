// Compact visual assets and small text resources, embedded once in the HTML.
window.crowbarAssets = (() => {
  const assets = JSON.parse(document.getElementById('embedded-assets').textContent);
  for (const image of document.querySelectorAll('[data-embedded-src]')) assets[image.dataset.embeddedSrc] = {url:image.src};
  const cache = new Map();
  async function blob(file) {
    const data = assets[file];
    if (!data) throw Error('This preview is unavailable.');
    const uri = data.url, binary = atob(uri.slice(uri.indexOf(',') + 1));
    return new Blob([Uint8Array.from(binary, c => c.charCodeAt(0))], {type:uri.slice(5, uri.indexOf(';'))});
  }
  async function url(file) {
    if (!cache.has(file)) cache.set(file, blob(file).then(URL.createObjectURL));
    return cache.get(file);
  }
  document.addEventListener('click', async event => {
    const link = event.target.closest('a[data-embedded-file]');
    if (!link) return;
    event.preventDefault();
    const file = link.dataset.embeddedFile;
    try {
      const save = document.createElement('a');
      save.href = await url(file);
      save.download = file.split('/').pop().replace(/\.png$/, '.webp');
      save.click();
    } catch {
      const toast = document.getElementById('toast');
      toast.textContent = 'This preview could not be saved. Try reopening the HTML file.';
      toast.classList.add('visible'); setTimeout(() => toast.classList.remove('visible'), 5000);
    }
  });
  return {url, blob};
})();
