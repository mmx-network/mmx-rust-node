import './style.css';

const root = document.documentElement;
const toggle = document.querySelector('[data-theme-toggle]');
const media = window.matchMedia('(prefers-color-scheme: light)');
let preference = null;
try { preference = localStorage.getItem('mmx-website-scheme'); } catch { /* Storage is optional. */ }

function applyScheme(scheme) {
  root.dataset.scheme = scheme;
  toggle.setAttribute('aria-label', `Switch to ${scheme === 'dark' ? 'light' : 'dark'} theme`);
  document.querySelector('meta[name="theme-color"]').content = scheme === 'dark' ? '#07060b' : '#f6f5fb';
}

applyScheme(preference === 'light' || preference === 'dark' ? preference : media.matches ? 'light' : 'dark');
toggle.hidden = false;
toggle.addEventListener('click', () => {
  preference = root.dataset.scheme === 'dark' ? 'light' : 'dark';
  applyScheme(preference);
  try { localStorage.setItem('mmx-website-scheme', preference); } catch { /* Keep working without storage. */ }
});
media.addEventListener('change', () => {
  if (preference !== 'dark' && preference !== 'light') applyScheme(media.matches ? 'light' : 'dark');
});
