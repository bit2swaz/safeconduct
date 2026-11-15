"""Synthetic shop with a fake backend. Live, it is nondeterministic on purpose:
random prices, random latency, a page that uses Math.random, crypto and Date.
Run: python3 sites/shop/server.py [port]"""
import json, random, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).parent
NAMES = ['kettle', 'lamp', 'mug', 'notebook', 'pen', 'scarf']
cart = []


class Shop(BaseHTTPRequestHandler):
    def reply(self, body, ctype='application/json'):
        time.sleep(random.random() * 0.05)
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(200)
        self.send_header('content-type', ctype)
        self.send_header('content-length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == '/':
            self.reply((HERE / 'index.html').read_bytes(), 'text/html')
        elif self.path == '/worker.js':
            self.reply(b"fetch('/api/stock').then((r) => r.json()).then((s) => postMessage(`${Math.floor(Math.random() * 1000)} stock ${s.stock}`))", 'text/javascript')
        elif self.path == '/api/stock':
            self.reply({'stock': random.randint(1, 99)})
        elif self.path == '/api/products':
            self.reply([{'id': n, 'price': round(random.uniform(2, 60), 2)} for n in NAMES])
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path == '/api/cart':
            cart.append(json.loads(self.rfile.read(int(self.headers['content-length']))))
            self.reply({'count': len(cart), 'order': random.randint(1000, 9999)})
        elif self.path == '/api/pay':
            self.rfile.read(int(self.headers['content-length']))
            self.reply({'paid': True})
        else:
            self.send_error(404)

    def log_message(self, *a):
        pass


ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1]) if len(sys.argv) > 1 else 8001), Shop).serve_forever()
