// Shared by the Node servers in bench/compare: the handlers' data, a WebSocket echo on /ws (the
// `ws` package), and running WORKERS processes with node:cluster, as PM2's cluster mode does.
'use strict';
const cluster = require('node:cluster');
const { WebSocketServer } = require('ws');

function items() {
  const out = [];
  for (let i = 0; i < 20; i++) out.push({ id: i, name: `item ${i}`, price: i * 1.5, tags: ['a', 'b'] });
  return out;
}

function user(id) {
  return { id, name: `user ${id}`, email: `user${id}@example.com`, active: id % 2 === 0 };
}

function receipt(order) {
  let total = 0;
  let count = 0;
  for (const line of order.lines) {
    total += line.qty * line.price;
    count += line.qty;
  }
  return { customer: order.customer, count, total };
}

function attachEcho(server) {
  const wss = new WebSocketServer({ server, path: '/ws' });
  wss.on('connection', (socket) => {
    socket.on('message', (data, isBinary) => socket.send(data, { binary: isBinary }));
  });
}

// Run `start(port)` in WORKERS processes (default 1: a single process, Node's default).
function run(start) {
  const port = Number(process.argv[2]);
  const workers = Number(process.env.WORKERS || 1);
  if (workers > 1 && cluster.isPrimary) {
    let ready = 0;
    for (let i = 0; i < workers; i++) {
      cluster.fork().on('message', () => {
        if (++ready === workers) console.error(`listening on ${port}`);
      });
    }
    process.on('SIGTERM', () => {
      for (const id in cluster.workers) cluster.workers[id].kill();
      process.exit(0);
    });
    return;
  }
  start(port, () => {
    if (process.send) process.send('ready');
    else console.error(`listening on ${port}`);
  });
}

module.exports = { items, user, receipt, attachEcho, run };
