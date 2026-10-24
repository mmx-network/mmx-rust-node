import { render } from 'preact';
import { applyAppearance, stylesReady } from '../../styles';
import './kitchen.css';
import { Kitchen } from './Kitchen';

const scheme = new URLSearchParams(location.search).get('scheme');
applyAppearance(scheme === 'dark' || scheme === 'light' ? scheme : 'system');

void stylesReady.then(() => {
  render(<Kitchen />, document.getElementById('app')!);
});
