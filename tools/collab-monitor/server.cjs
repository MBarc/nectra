'use strict';
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const host = '127.0.0.1';
const port = 4318;
const handoff = path.resolve(__dirname, '../../AGENT_HANDOFF.md');
const server = http.createServer(async (req, res) => {
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('X-Content-Type-Options', 'nosniff');
  res.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'");
  if (req.headers.host !== `${host}:${port}` && req.headers.host !== `localhost:${port}`) {
    res.writeHead(403).end('Invalid host'); return;
  }
  if (req.method !== 'GET') { res.writeHead(405, {Allow: 'GET'}).end('Method not allowed'); return; }
  try {
    if (req.url === '/api/state') {
      const [text, stat] = await Promise.all([fs.readFile(handoff, 'utf8'), fs.stat(handoff)]);
      res.writeHead(200, {'Content-Type': 'application/json; charset=utf-8'});
      res.end(JSON.stringify({handoff: text, updatedAt: stat.mtime.toISOString()}));
    } else if (req.url === '/' || req.url === '/index.html') {
      const html = await fs.readFile(path.join(__dirname, 'index.html'));
      res.writeHead(200, {'Content-Type': 'text/html; charset=utf-8'}).end(html);
    } else { res.writeHead(404).end('Not found'); }
  } catch (error) {
    console.error('Monitor read failed:', error.message);
    res.writeHead(503, {'Content-Type': 'application/json'}).end(JSON.stringify({error: 'Workspace data unavailable'}));
  }
});
server.on('error', error => { console.error(`Monitor failed: ${error.message}`); process.exitCode = 1; });
server.listen(port, host, () => console.log(`Nectra collaboration monitor: http://${host}:${port}`));
