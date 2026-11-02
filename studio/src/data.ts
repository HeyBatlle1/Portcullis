// The audit chain as AG Studio tables. One row per record, flattened; nothing is computed here that
// the record does not already say, except plain arithmetic (cents to dollars) and two 0/1 flags so
// counts can be summed.

import type { AgDataSourcesDefinition, AgFieldDefinition, AgFormat } from 'ag-studio';
import type { AuditRecord } from './chain';

const iso = (t: number) => new Date(t * 1000).toISOString();

type FieldSpec = [id: string, name: string, format: AgFormat, hide?: boolean, description?: string];
const USD = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });
const fields = (specs: FieldSpec[]): AgFieldDefinition[] =>
  specs.map(([id, name, format, hide, description]) => ({
    id,
    name,
    format,
    ...(format === 'currencyFormat' && { formatOptions: { format: USD } }),
    ...(hide && { hide }),
    ...(description && { description }),
  }));

const STEP: Record<string, string> = {
  submitted: 'Sent to PayPal',
  settled: 'Settled',
  released: 'Released, never paid',
  retrying: 'Retrying',
  credited_back: 'Money came back',
  unresolved: 'Held for a human',
  anomaly: 'Anomaly',
};

export function buildData(records: AuditRecord[]): AgDataSourcesDefinition {
  const decisions: Record<string, unknown>[] = [];
  const payouts: Record<string, unknown>[] = [];
  const approvals: Record<string, unknown>[] = [];

  for (const r of records) {
    let b: any;
    try {
      b = JSON.parse(r.body);
    } catch {
      continue;
    }
    if (r.kind === 'decision') {
      const p = b.proposal;
      const refuse = b.adjudication?.decision?.Refuse;
      const amount = Number(p.amount.minor) / 10 ** (p.amount.asset?.decimals ?? 2);
      decisions.push({
        seq: r.seq,
        at: iso(r.at),
        proposal: p.id,
        agent: p.agent,
        recipient: p.recipient,
        lookalike: [...String(p.recipient)].some((c) => c.charCodeAt(0) > 126 || c.charCodeAt(0) < 33),
        currency: p.amount.asset?.symbol ?? '',
        amount,
        bill: p.against ?? '(none)',
        outcome: refuse ? 'Refused' : 'Allowed',
        rule: refuse ? refuse.rule : '(allowed)',
        reason: b.reason ?? '',
        purpose: p.purpose ?? '',
        refused: refuse ? 1 : 0,
        allowed: refuse ? 0 : 1,
        refusedAmount: refuse ? amount : 0,
        allowedAmount: refuse ? 0 : amount,
      });
    } else if (r.kind === 'execution') {
      const e = b.event;
      payouts.push({
        seq: r.seq,
        at: iso(r.at),
        proposal: e.proposal,
        step: STEP[e.event] ?? e.event,
        batch: e.batch_id ?? '',
        reason: e.reason ?? e.detail ?? '',
      });
    } else if (r.kind === 'approval') {
      const c = typeof b.commitment === 'object' ? b.commitment : null;
      approvals.push({
        seq: r.seq,
        at: iso(r.at),
        bill: c ? c.id : b.commitment,
        action: b.action === 'approved' ? 'Approved' : 'Withdrawn',
        amount: c ? Number(c.earmarked) / 100 : 0,
        payee: c ? c.recipient : '',
      });
    }
  }

  return {
    description:
      'The Portcullis audit chain for Lakeshore Glazing, a fictional glazing subcontractor, on the PayPal sandbox. ' +
      'An AI agent proposes payments; a deterministic gate with no model in it allows or refuses each one by a named rule. ' +
      'Every row here is one hash-chained audit record. Money is USD.',
    sources: [
      {
        id: 'decisions',
        name: 'Decisions',
        description:
          'One row per payment the agent proposed, with the gate\'s answer. "refused"/"allowed" are 1 or 0, so sum them to count. ' +
          '"rule" names the policy rule that refused it. "purpose" is the agent\'s own words, which the gate never reads.',
        data: decisions,
        fields: fields([
          ['seq', 'Record #', 'integerFormat'],
          ['at', 'Decided at', 'dateTimeFormat'],
          ['proposal', 'Proposal ID', 'textFormat', true],
          ['agent', 'Agent', 'textFormat'],
          ['recipient', 'Recipient', 'textFormat'],
          ['lookalike', 'Lookalike characters', 'booleanFormat', false, 'True when the address contains characters that render like plain letters but are not.'],
          ['currency', 'Currency', 'textFormat'],
          ['amount', 'Amount', 'currencyFormat'],
          ['bill', 'Bill', 'textFormat'],
          ['outcome', 'Outcome', 'textFormat'],
          ['rule', 'Rule', 'textFormat'],
          ['reason', 'Reason', 'textFormat'],
          ['purpose', "Agent's reason", 'textFormat'],
          ['refused', 'Refusals', 'integerFormat'],
          ['allowed', 'Allowed', 'integerFormat'],
          ['refusedAmount', 'Amount refused', 'currencyFormat'],
          ['allowedAmount', 'Amount allowed', 'currencyFormat'],
        ]),
      },
      {
        id: 'payouts',
        name: 'Payout steps',
        description: 'What happened at PayPal after an allowed decision: sent, settled, released, retried.',
        data: payouts,
        fields: fields([
          ['seq', 'Record #', 'integerFormat'],
          ['at', 'At', 'dateTimeFormat'],
          ['proposal', 'Proposal ID', 'textFormat', true],
          ['step', 'Step', 'textFormat'],
          ['batch', 'PayPal batch', 'textFormat'],
          ['reason', 'Detail', 'textFormat'],
        ]),
      },
      {
        id: 'approvals',
        name: 'Approvals',
        description: 'The owner approving or withdrawing a bill. Only an approved bill can be paid against.',
        data: approvals,
        fields: fields([
          ['seq', 'Record #', 'integerFormat'],
          ['at', 'At', 'dateTimeFormat'],
          ['bill', 'Bill', 'textFormat'],
          ['action', 'Action', 'textFormat'],
          ['amount', 'Amount', 'currencyFormat'],
          ['payee', 'Payee', 'textFormat'],
        ]),
      },
    ],
    relationships: [
      {
        id: 'payout-decision',
        source: { tableId: 'payouts', fieldId: 'proposal' },
        target: { tableId: 'decisions', fieldId: 'proposal' },
        type: 'many-to-one',
      },
    ],
  } as AgDataSourcesDefinition;
}
