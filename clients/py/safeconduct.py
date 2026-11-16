"""safeconduct Python client: start the replay browser, connect your agent, and check rules after every step.

    with replay('run.jsonl', rules=[allowed_origins(['http://127.0.0.1:8001'])]) as run:
        ...  # connect the agent to run.endpoint (CDP) and run it
    run.violations

A step ends when the agent acted (navigate, input, script), the page sent requests, and the
network went quiet again. The runtime prints a step event then; the rules run on it.
Requests inside one step can arrive in any order. Rules run on a reader thread.
"""
import atexit, json, os, signal, subprocess, threading
from dataclasses import dataclass
from urllib.parse import urlsplit


@dataclass
class Step:
    n: int
    events: list  # the events since the previous step


@dataclass
class Violation:
    rule: str
    step: int
    message: str


class Run:
    """A running safeconduct runtime. A rule is `rule(step, events) -> str | None`; events = every event so far."""

    def __init__(self, args, rules=(), port=None, seed=None, bin=None):
        args += ['--port', str(port)] if port is not None else []
        args += ['--seed', str(seed)] if seed is not None else []
        bin = bin or os.environ.get('SAFECONDUCT_BIN', 'safeconduct')
        self.proc = subprocess.Popen([bin, *args], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True)
        atexit.register(self._kill)
        self.rules, self.events, self.violations, self.steps, self._from = list(rules), [], [], 0, 0
        self.fingerprint = ''  # a hash of what the agent did (each step's requests), set when the runtime ends
        for line in self.proc.stdout:
            if line.startswith('{') and (e := json.loads(line))['event'] == 'ready':
                self.endpoint, self.llm = e['endpoint'], e['llm']
                break
        else:
            raise RuntimeError('safeconduct exited before it was ready')
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()

    def _read(self):
        for line in self.proc.stdout:
            if not line.startswith('{'):
                continue
            e = json.loads(line)
            if e['event'] == 'step':
                self._check()
            elif e['event'] == 'end':
                self.fingerprint = e['fingerprint']
            else:
                self.events.append(e)

    def _check(self):
        self.steps += 1
        step = Step(self.steps, self.events[self._from:])
        self._from = len(self.events)
        for rule in self.rules:
            if message := rule(step, self.events):
                self.violations.append(Violation(rule.__name__, step.n, message))

    def _kill(self):
        if self.proc.poll() is None:
            self.proc.terminate()  # the runtime closes Chrome on SIGTERM

    def close(self):
        """Stop the runtime, check the last step, and return its exit code (3 = left the recording)."""
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGINT)
        code = self.proc.wait()
        self._reader.join()
        atexit.unregister(self._kill)
        if len(self.events) > self._from:
            self._check()  # the last step has no step event after it
        return code

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def record(file, **opts):
    return Run(['record', file], **opts)


def replay(file, live_model=False, fuzz=None, fault_rate=0.25, **opts):
    args = ['replay', file] + (['--live-model'] if live_model else [])
    args += ['--fuzz', str(fuzz), '--fault-rate', str(fault_rate)] if fuzz is not None else []
    return Run(args, **opts)


def allowed_origins(origins):
    """Every request goes to one of these origins, e.g. 'http://127.0.0.1:8001'."""
    def allowed_origins(step, events):
        for e in step.events:
            u = urlsplit(e.get('url', ''))
            if e['event'] == 'request' and f'{u.scheme}://{u.netloc}' not in origins:
                return f"left the allowed origins: {e['method']} {e['url']}"
    return allowed_origins


def no_double_payment(path):
    """A payment (a POST to a URL that contains `path`) is never sent twice with the same body."""
    def pay(e):
        return e['event'] == 'request' and e['method'] == 'POST' and path in e['url']

    def no_double_payment(step, events):
        for e in step.events:
            if pay(e) and sum(1 for a in events if pay(a) and a['url'] == e['url'] and a.get('post') == e.get('post')) > 1:
                return f"payment sent twice: POST {e['url']} {e.get('post', '')}"
    return no_double_payment
