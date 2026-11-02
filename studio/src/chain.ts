// The audit chain, re-verified here rather than trusted.
//
// Same encoding as crates/kernel/src/audit.rs: the domain tag "portcullis.audit.v1", then each of
// seq, at, kind, body and prev as an 8-byte big-endian length followed by its bytes. The console
// page does the same walk; this is the module form of it.

export interface AuditRecord {
  seq: number;
  at: number;
  kind: string;
  body: string;
  prev: string;
  hash: string;
}

const enc = new TextEncoder();

function u64be(n: number): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(n));
  return b;
}

function unhex(h: string): Uint8Array {
  const b = new Uint8Array(h.length / 2);
  for (let i = 0; i < b.length; i++) b[i] = parseInt(h.substr(i * 2, 2), 16);
  return b;
}

function tohex(buf: ArrayBuffer): string {
  return [...new Uint8Array(buf)].map((x) => x.toString(16).padStart(2, '0')).join('');
}

export async function digest(r: AuditRecord): Promise<string> {
  const parts: Uint8Array[] = [enc.encode('portcullis.audit.v1')];
  for (const f of [u64be(r.seq), u64be(r.at), enc.encode(r.kind), enc.encode(r.body), unhex(r.prev)]) {
    parts.push(u64be(f.length), f);
  }
  const all = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) {
    all.set(p, o);
    o += p.length;
  }
  return tohex(await crypto.subtle.digest('SHA-256', all));
}

export interface ChainStatus {
  /** Records checked so far, all of them in order. */
  checked: number;
  /** Where the chain stopped holding together, if it did. */
  broken: string | null;
  head: string | null;
  /** The first record we hold; non-zero means the walk started mid-chain. */
  from: number;
}

/** Walk `records[from..]` given what was already checked. Returns the new status. */
export async function verifyFrom(records: AuditRecord[], status: ChainStatus): Promise<ChainStatus> {
  let { checked, broken } = status;
  while (checked < records.length && !broken) {
    const r = records[checked];
    const prev = records[checked - 1];
    if (prev && (r.prev !== prev.hash || r.seq !== prev.seq + 1)) broken = `link broken at record ${r.seq}`;
    else if (prev && r.at < prev.at) broken = `time ran backwards at record ${r.seq}`;
    else if (!prev && r.seq === 0 && !/^0{64}$/.test(r.prev)) broken = 'the first record does not start the chain';
    else if ((await digest(r)) !== r.hash) broken = `record ${r.seq} does not match its hash`;
    checked++;
  }
  return { checked, broken, head: records.at(-1)?.hash ?? null, from: records[0]?.seq ?? 0 };
}
