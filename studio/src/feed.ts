// The live feed: the whole chain once from /console/audit.jsonl, then every new record and every
// state change from /console/stream. Nothing here writes; the server exposes nothing to write to.

import { useSyncExternalStore } from 'react';
import { type AuditRecord, type ChainStatus, verifyFrom } from './chain';

/** The gate as the server computes it. Every number is the kernel's. */
export interface GateState {
  now: number;
  currency: string;
  balance: string;
  reserved: string;
  encumbered: string;
  available: string;
  policy: {
    version: number;
    head: string;
    rules: Record<string, any>[];
    standing: { recipient: string; active_from: number }[];
  };
  commitments: { id: string; recipient: string; amount: string; state: string; closes: number }[];
}

export interface Feed {
  records: AuditRecord[];
  chain: ChainStatus;
  state: GateState | null;
  connected: boolean;
  /** Bumped on every change, so a snapshot comparison is cheap. */
  version: number;
}

type Listener = () => void;
type RecordListener = (r: AuditRecord) => void;

let feed: Feed = {
  records: [],
  chain: { checked: 0, broken: null, head: null, from: 0 },
  state: null,
  connected: false,
  version: 0,
};
const listeners = new Set<Listener>();
const recordListeners = new Set<RecordListener>();
let started = false;
let verifying: Promise<void> = Promise.resolve();

function publish(next: Partial<Feed>) {
  feed = { ...feed, ...next, version: feed.version + 1 };
  listeners.forEach((l) => l());
}

function add(r: AuditRecord, live: boolean) {
  const last = feed.records.at(-1);
  if (last && r.seq <= last.seq) return;
  feed.records = [...feed.records, r];
  publish({});
  verifying = verifying.then(async () => publish({ chain: await verifyFrom(feed.records, feed.chain) }));
  if (live) recordListeners.forEach((l) => l(r));
}

export function start() {
  if (started) return;
  started = true;
  fetch('/console/audit.jsonl')
    .then((r) => r.text())
    .then((text) => {
      for (const line of text.split('\n')) if (line.trim()) add(JSON.parse(line), false);
      connect();
    })
    .catch(connect);
}

function connect() {
  const es = new EventSource('/console/stream');
  es.onopen = () => publish({ connected: true });
  es.onerror = () => publish({ connected: false });
  es.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.ch === 'hello') {
      // A restarted server begins a new chain. Start over rather than splice two histories.
      const first = msg.records[0];
      if (first && feed.records.length && first.seq === 0 && first.hash !== feed.records[0].hash) {
        feed.records = [];
        feed.chain = { checked: 0, broken: null, head: null, from: 0 };
      }
      for (const r of msg.records) add(r, false);
      publish({ state: msg.state });
    } else if (msg.ch === 'audit') add(msg.record, true);
    else if (msg.ch === 'state') publish({ state: msg.state });
    else if (msg.ch === 'lagged') {
      es.close();
      connect();
    }
  };
}

export function useFeed(): Feed {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => feed,
  );
}

export function current(): Feed {
  return feed;
}

/** Called for each record that arrives live, after the initial load. */
export function onRecord(l: RecordListener): () => void {
  recordListeners.add(l);
  return () => recordListeners.delete(l);
}
