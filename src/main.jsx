import React from 'react';
import ReactDOM from 'react-dom/client';
import './i18n/index.js';
import App from './App.jsx';
import './styles.css';

window.addEventListener('error', (e) => {
  document.body.innerHTML = '<pre style="padding:20px;font:14px monospace;color:red">ERROR: ' + (e.error?.stack || e.message) + '</pre>';
});
window.addEventListener('unhandledrejection', (e) => {
  document.body.innerHTML = '<pre style="padding:20px;font:14px monospace;color:red">REJECTION: ' + (e.reason?.stack || e.reason) + '</pre>';
});

try {
  ReactDOM.createRoot(document.getElementById('root')).render(<App />);
} catch (e) {
  document.body.innerHTML = '<pre style="padding:20px;font:14px monospace;color:red">RENDER ERROR: ' + (e.stack || e.message) + '</pre>';
}
