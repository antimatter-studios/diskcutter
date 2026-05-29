#!/usr/bin/env node
// HTTPS dev update server.
// Cert/key are embedded — nothing written to disk, nothing installed anywhere.
// The app accepts this self-signed cert because danger_accept_invalid_certs is
// set for localhost endpoints in updater.rs.
// Accepts GET (serve) and PUT (upload) against DATA_DIR.

import https from 'https';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const PORT = parseInt(process.env.PORT || '17780');
const DATA_DIR = process.env.DATA_DIR || path.join(process.cwd(), 'dev-updates');

fs.mkdirSync(DATA_DIR, { recursive: true });

const KEY = `-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWE+uAse8AO1CiZga
ddXvNfDMRBc1lMfkO9fC8bHSOPGhRANCAASeHnK09fJkcpPDhClYunbn4rCarxO8
BA0Rzx/nVC9oeOUmKH3wfKmWe0BriPuBU/F7jU6NDmnrG9d4ohMqC0kc
-----END PRIVATE KEY-----`;

const CERT = `-----BEGIN CERTIFICATE-----
MIIBvjCCAWWgAwIBAgIUPeAaIvtUCBm0Zl/p2eBAVz6zbW4wCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDUyOTEyNDk0NFoYDzIxMjYwNTA1
MTI0OTQ0WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjO
PQMBBwNCAASeHnK09fJkcpPDhClYunbn4rCarxO8BA0Rzx/nVC9oeOUmKH3wfKmW
e0BriPuBU/F7jU6NDmnrG9d4ohMqC0kco4GSMIGPMB0GA1UdDgQWBBTbPtNtI8L/
uPj3hOhXzfGbfi2o2DAfBgNVHSMEGDAWgBTbPtNtI8L/uPj3hOhXzfGbfi2o2DAa
BgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwDAYDVR0TAQH/BAIwADAOBgNVHQ8B
Af8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUHAwEwCgYIKoZIzj0EAwIDRwAwRAIg
DbzlRrhNnb3PkuIbjkNSfWixovJAS7cndruDobq45tECICW0NP8Pg1mollKghJqR
mBfvgf6Xp15mk81iDlIFfmNx
-----END CERTIFICATE-----`;

https.createServer({ key: KEY, cert: CERT }, (req, res) => {
  const name = path.basename(req.url.split('?')[0]);
  if (!name) { res.writeHead(400); res.end(); return; }
  const file = path.join(DATA_DIR, name);

  if (req.method === 'GET' || req.method === 'HEAD') {
    try {
      const data = fs.readFileSync(file);
      res.writeHead(200, { 'Content-Length': data.length });
      if (req.method === 'GET') res.end(data); else res.end();
    } catch { res.writeHead(404); res.end('not found\n'); }
  } else if (req.method === 'PUT') {
    const ws = fs.createWriteStream(file);
    req.pipe(ws);
    ws.on('finish', () => {
      res.writeHead(200);
      res.end('OK\n');
      process.stdout.write(`← PUT ${name} (${ws.bytesWritten} bytes)\n`);
    });
    ws.on('error', e => { res.writeHead(500); res.end(e.message + '\n'); });
  } else {
    res.writeHead(405); res.end();
  }
}).listen(PORT, '127.0.0.1', () => {
  process.stdout.write(`→ https://127.0.0.1:${PORT}/  serving ${DATA_DIR}\n`);
});
