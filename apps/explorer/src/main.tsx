import { render } from 'preact';
import { applyAppearance, stylesReady } from '@wallet/styles';
import { App, storedAppearance } from './App';
import './styles.css';

applyAppearance(storedAppearance());
void stylesReady.then(() => render(<App />, document.getElementById('app')!));
