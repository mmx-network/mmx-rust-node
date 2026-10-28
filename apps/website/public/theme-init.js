// Classic, parser-blocking head script: choose the theme before body/CSS paint.
(() => {
  let preference = null;
  try { preference = localStorage.getItem('mmx-website-scheme'); } catch { /* Storage is optional. */ }
  const scheme = preference === 'light' || preference === 'dark'
    ? preference
    : window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  document.documentElement.dataset.scheme = scheme;
  document.querySelector('meta[name="theme-color"]').content = scheme === 'light' ? '#f6f5fb' : '#07060b';
})();
