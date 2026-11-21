// Fastify 5 server for bench/compare, with JSON schemas so it serializes with its fast
// stringifier, as Fastify recommends. Usage: [WORKERS=n] node fastify.js PORT
'use strict';
const Fastify = require('fastify');
const { items, user, receipt, attachEcho, run } = require('./common');

const itemSchema = {
  type: 'object',
  properties: { id: { type: 'integer' }, name: { type: 'string' }, price: { type: 'number' }, tags: { type: 'array', items: { type: 'string' } } },
};

run((port, ready) => {
  const app = Fastify({ logger: false });
  app.get('/json', { schema: { response: { 200: { type: 'object', properties: { message: { type: 'string' } } } } } }, async () => ({ message: 'Hello, World!' }));
  app.get('/items', { schema: { response: { 200: { type: 'array', items: itemSchema } } } }, async () => items());
  app.get('/users/:id', {
    schema: {
      params: { type: 'object', properties: { id: { type: 'integer' } } },
      response: { 200: { type: 'object', properties: { id: { type: 'integer' }, name: { type: 'string' }, email: { type: 'string' }, active: { type: 'boolean' } } } },
    },
  }, async (req) => user(req.params.id));
  app.post('/orders', {
    schema: {
      body: {
        type: 'object',
        required: ['customer', 'lines'],
        properties: {
          customer: { type: 'string' },
          lines: { type: 'array', items: { type: 'object', required: ['sku', 'qty', 'price'], properties: { sku: { type: 'string' }, qty: { type: 'integer' }, price: { type: 'number' } } } },
        },
      },
      response: { 200: { type: 'object', properties: { customer: { type: 'string' }, count: { type: 'integer' }, total: { type: 'number' } } } },
    },
  }, async (req) => receipt(req.body));
  attachEcho(app.server);
  app.listen({ port, host: '127.0.0.1' }).then(ready);
});
