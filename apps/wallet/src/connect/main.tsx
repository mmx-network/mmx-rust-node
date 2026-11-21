import { render } from 'preact';
import { stylesReady } from '../styles';
import { ConnectApp } from './ConnectApp';
import './connect.css';
const root = document.getElementById('app');
if (root) void stylesReady.then(() => render(<ConnectApp />, root));
