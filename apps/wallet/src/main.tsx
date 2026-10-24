import { render } from 'preact';
import { App } from './App';
import { stylesReady } from './styles';

const root = document.getElementById('app');
if (root) {
  // Render once the theme stylesheet is in, so the first frame already has the right colors.
  void stylesReady.then(() => render(<App />, root));
}
