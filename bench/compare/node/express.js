// Express 4 server for bench/compare. Usage: [WORKERS=n] node express.js PORT
'use strict';
const http = require('node:http');
const express = require('express');
const { items, user, receipt, attachEcho, run } = require('./common');

run((port, ready) => {
  const app = express();
  app.disable('x-powered-by');
  app.disable('etag');
  app.use(express.json());
  app.get('/json', (req, res) => res.json({ message: 'Hello, World!' }));
  app.get('/items', (req, res) => res.json(items()));
  app.get('/users/:id', (req, res) => {
    const id = Number.parseInt(req.params.id, 10);
    if (Number.isNaN(id)) return res.status(400).send('id must be an integer');
    res.json(user(id));
  });
  app.post('/orders', (req, res) => res.json(receipt(req.body)));
  const server = http.createServer(app);
  attachEcho(server);
  server.keepAliveTimeout = 65000;
  server.listen(port, '127.0.0.1', ready);
});
