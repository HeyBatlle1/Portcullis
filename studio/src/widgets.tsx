// Three widgets AG Studio does not ship, each for something only this dashboard has:
//
//   refusal-stamps  the refusals, counted per rule, drawn as the rubber stamps the console uses
//   chain-seal      whether the audit chain verified in this browser, and its head hash
//   headroom        how much of the rolling spend limit is used, and how far above the floor
//
// The stamps read their numbers through Studio's query engine, so page filters and cross-filters
// apply to them like any other widget. The seal and the headroom read the live feed instead: the
// chain's integrity and the gate's limits are not rows in a table.

import { useEffect, useRef, useState } from 'react';
import type {
  AgBaseRegistry,
  AgBaseWidgetDefinition,
  AgDefaultRegistry,
  AgWidgetDataFormat,
  AgWidgetFieldReference,
  AgWidgetFormParams,
  AgWidgetParams,
  AgWidgetsConfig,
} from 'ag-studio';
import { createWidgets } from 'ag-studio-react';
import { useFeed } from './feed';

export interface StampsDef {
  type: 'refusal-stamps';
  dataMapping: { rule: AgWidgetFieldReference[]; count: AgWidgetFieldReference[] };
  format?: AgWidgetDataFormat<object>;
}
export interface SealDef {
  type: 'chain-seal';
  dataMapping: Record<string, never>;
  format?: AgWidgetDataFormat<object>;
}
export interface HeadroomDef {
  type: 'headroom';
  dataMapping: Record<string, never>;
  format?: AgWidgetDataFormat<object>;
}

export interface DeskRegistry extends AgBaseRegistry {
  widgets: readonly (
    | AgBaseWidgetDefinition<'refusal-stamps', StampsDef>
    | AgBaseWidgetDefinition<'chain-seal', SealDef>
    | AgBaseWidgetDefinition<'headroom', HeadroomDef>
  )[];
}

const RULE_WORDS: Record<string, string> = {
  CommitmentRecipientMismatch: 'Bill pinned to another payee',
  RecipientNotAllowed: 'Payee not on the list',
  RecipientMalformed: 'Not a real address',
  RecipientNotYetActive: 'Payee still timelocked',
  ExceedsPerTransaction: 'Over the per-payment limit',
  ExceedsRollingWindow: 'Over the 24-hour limit',
  ExceedsVelocity: 'Too many payments',
  BreachesBalanceFloor: 'Below the balance floor',
  AssetMismatch: 'Wrong currency',
  AssetNotAllowed: 'Currency not allowed',
  FundsEncumbered: 'Money set aside for a bill',
  InsufficientFunds: 'Not enough money',
  DuplicateProposal: 'Already paid',
  CommitmentNotFound: 'No such bill',
  CommitmentNotOpen: 'Bill already paid or withdrawn',
  CommitmentWindowClosed: 'Bill not payable now',
  CommitmentAmountExceeded: 'More than the bill',
  CommitmentRequired: 'Not an approved bill',
  OutsideTimeWindow: 'Outside allowed hours',
};

// ── refusal stamps ──────────────────────────────────────────────────────────

function Stamps({ dataMapping, widgetApi }: AgWidgetParams<StampsDef>) {
  const [rows, setRows] = useState<{ rule: string; n: number }[]>([]);
  const loaded = useRef(false);
  useEffect(() => {
    const rule = dataMapping.rule?.at(0);
    const count = dataMapping.count?.at(0);
    if (!rule || !count) {
      widgetApi.setDisplayState('incompleteDataMapping');
      return;
    }
    let live = true;
    (async () => {
      widgetApi.setDisplayState('loading', { prominent: !loaded.current });
      const res = await widgetApi.getData({ fields: [rule, count] });
      if (!live) return;
      const out = res.results.rows
        .map((r: any) => ({ rule: String(r[rule.key] ?? ''), n: Number(r[count.key] ?? 0) }))
        .filter((r: { rule: string; n: number }) => r.n > 0 && r.rule !== '(allowed)')
        .sort((a: { n: number }, b: { n: number }) => b.n - a.n);
      loaded.current = true;
      setRows(out);
      widgetApi.setDisplayState(out.length ? 'displayed' : 'noData');
    })();
    return () => {
      live = false;
    };
  }, [dataMapping, widgetApi]);

  return (
    <div className="stamps">
      {rows.map((r, i) => (
        <div className="stamp-cell" key={r.rule}>
          <div className="stamp" style={{ transform: `rotate(${[-8, -4, -10, -6, -3][i % 5]}deg)` }}>
            <b>{r.n}</b>
            <span>Refused</span>
          </div>
          <div className="stamp-rule">
            {RULE_WORDS[r.rule] ?? r.rule}
            <code>{r.rule}</code>
          </div>
        </div>
      ))}
    </div>
  );
}

// ── chain seal ──────────────────────────────────────────────────────────────

function Seal({ widgetApi }: AgWidgetParams<SealDef>) {
  const { records, chain } = useFeed();
  useEffect(() => {
    widgetApi.setDisplayState('displayed');
  }, [widgetApi]);
  const done = chain.checked === records.length;
  const ok = !chain.broken;
  return (
    <div className={`seal ${ok ? 'ok' : 'bad'}`}>
      <div className="seal-ring" aria-hidden="true">
        <svg viewBox="0 0 48 48">
          <circle cx="24" cy="24" r="21" />
          {ok ? <path d="M14 25l7 7 13-15" /> : <path d="M16 16l16 16M32 16L16 32" />}
        </svg>
      </div>
      <div className="seal-text">
        <strong>
          {!records.length
            ? 'No records yet'
            : chain.broken
              ? `Chain broken: ${chain.broken}`
              : done
                ? `Verified in this browser: ${records.length} records`
                : `Verifying… ${chain.checked} of ${records.length}`}
        </strong>
        <span>Each record's SHA-256 was recomputed here and linked to the one before it.</span>
        {chain.head && <code>head {chain.head}</code>}
      </div>
    </div>
  );
}

// ── headroom ────────────────────────────────────────────────────────────────

function Headroom({ widgetApi }: AgWidgetParams<HeadroomDef>) {
  const { state, records } = useFeed();
  useEffect(() => {
    widgetApi.setDisplayState(state ? 'displayed' : 'loading');
  }, [state, widgetApi]);
  if (!state) return null;

  const rule = (k: string) => state.policy.rules.find((r) => k in r)?.[k];
  const rolling = rule('RollingWindowMax');
  const floor = rule('BalanceFloor');
  const perTx = rule('PerTransactionMax');
  const now = state.now;
  // What the rolling limit counts: allowed decisions in the window that were not released.
  const released = new Set<string>();
  for (const r of records) {
    if (r.kind !== 'execution') continue;
    const e = JSON.parse(r.body).event;
    if (e.event === 'released') released.add(e.proposal);
  }
  let spent = 0;
  if (rolling) {
    for (const r of records) {
      if (r.kind !== 'decision' || r.at < now - rolling.window) continue;
      const b = JSON.parse(r.body);
      if (b.adjudication.decision.Allow && !released.has(b.proposal.id)) spent += Number(b.proposal.amount.minor);
    }
  }
  const usd = (minor: number) =>
    (minor / 100).toLocaleString('en-US', { style: 'currency', currency: 'USD' });
  const balance = Math.round(Number(state.balance) * 100);
  const bar = (used: number, of: number) => Math.max(0, Math.min(100, (used / of) * 100));

  return (
    <div className="headroom">
      {rolling && (
        <div className="meter">
          <div className="meter-label">
            <span>Spent in the last {rolling.window / 3600} hours</span>
            <b>
              {usd(spent)} <em>of {usd(rolling.minor)}</em>
            </b>
          </div>
          <div className="meter-track">
            <div className="meter-fill" style={{ width: `${bar(spent, rolling.minor)}%` }} />
          </div>
        </div>
      )}
      {floor && (
        <div className="meter">
          <div className="meter-label">
            <span>Balance above the floor</span>
            <b>
              {usd(balance - floor.minor)} <em>floor {usd(floor.minor)}</em>
            </b>
          </div>
          <div className="meter-track">
            <div className="meter-fill calm" style={{ width: `${bar(balance - floor.minor, balance)}%` }} />
          </div>
        </div>
      )}
      <div className="headroom-foot">
        {perTx && <span>No single payment over {usd(perTx.minor)}</span>}
        <span>Agent may spend {Number(state.available).toLocaleString('en-US', { style: 'currency', currency: 'USD' })} now</span>
      </div>
    </div>
  );
}

// ── registration ────────────────────────────────────────────────────────────

const ICON =
  '<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M3 4h18M3 4v17M21 4v17M8 4v18M13 4v18M18 4v18M3 10h18M3 16h18" stroke="currentColor" fill="none" stroke-width="1.6"/></svg>';

const plainForm = (p: AgWidgetFormParams<any>) => p.createDefaults({});

export const widgets = (base: AgWidgetsConfig<AgDefaultRegistry>) =>
  createWidgets<DeskRegistry>({
    additionalTypes: [
      {
        id: 'refusal-stamps',
        label: 'Refusal stamps',
        icon: ICON,
        dataMapping: {
          rule: {
            type: 'field',
            supportedRoles: ['category'],
            requires: { cardinality: 'many' },
            required: true,
            aiDescription: 'The rule that refused the payment, usually decisions.rule.',
          },
          count: {
            type: 'field',
            supportedRoles: ['numeric'],
            requires: { per: 'dataMapping.rule', cardinality: 'one' },
            required: true,
            aiDescription: 'How many refusals, usually the sum of decisions.refused.',
          },
        },
        form: (p: AgWidgetFormParams<StampsDef>) =>
          p.createDefaults({
            dataMappingItems: [
              { key: 'rule', label: 'Rule' },
              { key: 'count', label: 'Count' },
            ],
          }),
        comp: Stamps,
        defaultSize: { width: 480, height: 300 },
        minSize: { width: 260, height: 180 },
        ai: {
          description: 'Refusals counted per policy rule, drawn as rubber stamps. Map rule to decisions.rule and count to sum of decisions.refused.',
        },
      },
      {
        id: 'chain-seal',
        label: 'Chain seal',
        icon: ICON,
        form: plainForm,
        comp: Seal,
        defaultSize: { width: 420, height: 180 },
        minSize: { width: 260, height: 120 },
        ai: { description: 'Shows whether the audit chain verified in this browser, and its head hash. Takes no data.' },
      },
      {
        id: 'headroom',
        label: 'Headroom',
        icon: ICON,
        form: plainForm,
        comp: Headroom,
        defaultSize: { width: 420, height: 220 },
        minSize: { width: 260, height: 160 },
        ai: { description: "How much of the gate's rolling spend limit is used and how far the balance is above its floor. Takes no data." },
      },
    ] as any,
    menu: [...(base.menu as any), { label: 'Portcullis', widgetIds: ['refusal-stamps', 'chain-seal', 'headroom'] }],
  });
