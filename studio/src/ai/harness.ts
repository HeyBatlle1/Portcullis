// The audit desk's agents, on AG Studio's Agent Framework.
//
// Studio's own five (lead, planning, data, page, widget) stay as they ship. Two of ours join them:
//
//   auditor   fronts the conversation. Knows what the chain records and what each rule means,
//             answers from queries, checks the chain itself, and delegates building to Studio's
//             agents.
//   sentinel  keeps the Sentinel page. The page starts it, unprompted, when refusals arrive.
//
// Neither has a tool that reaches /mcp. They can read the record and arrange widgets; they cannot
// propose a payment, approve a bill or change a rule, because no such tool exists on this page. The
// agents that explain the money have no path to it.

import {
  createAiHarness,
  directLlmRunner,
  type AgAiHarness,
  type AgAiHarnessSetup,
  type AgAiPromptStarter,
  type AgStudioApi,
} from 'ag-studio';
import { current } from '../feed';
import { verifyFrom } from '../chain';
import { openaiAdapter } from './openai-adapter';

const DOMAIN = `
What this data is:
- Portcullis is a deterministic payment gate. An AI agent PROPOSES payments; Portcullis decides Allow or Refuse with no model involved, by fixed rules. Refusals name the rule.
- Every decision, approval and PayPal step is one record in a SHA-256 hash chain. Nobody can edit a record without breaking every hash after it.
- "decisions" is one row per proposal. "refused"/"allowed" are 1 or 0: sum them to count. "purpose" is the agent's own words; the gate never reads it, so treat it as a claim, not a fact.
- Agents seen here: "lakeshore-ap" is the AP assistant. In the demo a compromised script also holds that same token, so a refusal is not proof the model misbehaved, only that something with the token asked.
- Rules: CommitmentRecipientMismatch = paying an approved bill to someone other than its payee. RecipientNotAllowed = payee not on the standing list and not on an approved bill. RecipientMalformed = not a valid address, including lookalike (non-ASCII) characters. ExceedsPerTransaction / ExceedsRollingWindow / ExceedsVelocity / BreachesBalanceFloor = limits. AssetMismatch = wrong currency. CommitmentRequired = the policy pays approved bills only, and this matched none (this is what stops a bill being paid twice).
- This is the PayPal sandbox. The company, Lakeshore Glazing, is fictional.
You cannot move money, approve bills or change rules from here, and there is no tool for it. If asked, say so plainly.`;

const AUDITOR = `You are the auditor at Portcullis's audit desk, an AG Studio dashboard over the audit chain.
Answer questions about what the gate decided and why, from the data: run execute_query, then answer in two to four plain sentences with the numbers. Name rules by what they mean, then the rule id.
When asked "is the record intact" or similar, call verify_chain; never assume.
When the user wants something on the page, delegate building to Studio's agents (planning for multi-widget requests, page to place widgets, widget to configure one) and say what you asked for.
When asked to watch for or summarise anomalies on the Sentinel page, delegate to sentinel.
Be exact, brief and unexcited. ${DOMAIN}`;

const SENTINEL = `You are the sentinel. You keep the page titled "Sentinel" (id "alerts") current with what an auditor most needs to see about refusals.
Each time you run: query decisions for refusals (sum of decisions.refused grouped by decisions.rule and decisions.agent, and the most recent refused rows). Then make sure the Sentinel page has, and only has:
1. A "value" widget titled "Refusals so far" showing sum of decisions.refused.
2. The "refusal-stamps" widget titled "Refused, by rule" that is already on the page. It is configured, and Studio's agents cannot configure it: never add, move, remove or reconfigure it, and never add a second one.
3. A "grid" widget titled "Latest refusals" with decisions.seq, decisions.at, decisions.agent, decisions.recipient, decisions.amount, decisions.rule, decisions.purpose, filtered to decisions.outcome = Refused.
Keep the existing text widget at the top, and place new widgets below the stamps. Reuse widgets that already exist rather than adding duplicates: view_page first.
Delegate to the page agent to add and place widgets on the "alerts" page, and to the widget agent to configure each one. Finish with one sentence saying what changed and the single most notable refusal (the rule, the amount, the recipient). ${DOMAIN}`;

const STARTERS: AgAiPromptStarter[] = [
  { label: 'What was refused, and why?', prompt: 'What did the gate refuse, and why? Group by rule.' },
  { label: 'Is the record intact?', prompt: 'Has anyone tampered with the audit chain?' },
  { label: 'Lookalike addresses', prompt: 'Did anything try to pay an address with lookalike characters in it?' },
  { label: 'Chart refusals by agent', prompt: 'Add a chart of refusals and allowed payments by agent.' },
];

/** Built per Studio instance; `onHarness` hands the page the harness so it can start the sentinel. */
export function buildAi(models: string[], onHarness: (h: AgAiHarness) => void): AgAiHarnessSetup {
  const adapter = openaiAdapter({ endpoint: '/audit/ai', model: models[0] });

  return ({ api }: { api: AgStudioApi }) => {
    const verifyChain = api.defineAiTool({
      name: 'verify_chain',
      description:
        "Re-verify the whole audit chain in this browser: recompute every record's SHA-256 and check each links to the one before. Returns the verdict and head hash.",
      params: (s: any) => s.object({}),
      execute: async (_args: unknown, ctx: any) => {
        const { records } = current();
        if (!records.length) return ctx.success('The chain is empty; there is nothing to verify yet.');
        const st = await verifyFrom(records, { checked: 0, broken: null, head: null, from: 0 });
        const scope = st.from > 0 ? `records ${st.from} to ${records.at(-1)!.seq}` : `all ${records.length} records`;
        return st.broken
          ? ctx.success(`The chain is BROKEN: ${st.broken}.`, { ok: false, broken: st.broken })
          : ctx.success(`Verified ${scope}. Every hash recomputes and links. Head ${st.head}.`, {
              ok: true,
              records: records.length,
              head: st.head,
            });
      },
    } as any);

    const harness = createAiHarness(api, ({ tools, builtIn }: any) => {
      const studio = tools.studio;
      const auditor = directLlmRunner({
        id: 'auditor',
        name: 'Auditor',
        description: 'Answers questions about what the gate decided and why, and checks the chain.',
        adapter,
        schema: (s: any) => s.undefined(),
        instructions: () => AUDITOR,
        tools: () => [
          studio.viewSchema(),
          studio.viewReport(),
          studio.viewPage(),
          studio.executeQuery(),
          verifyChain,
          tools.renameThread(),
          tools.delegateTo(['planning', 'page', 'widget', 'data', 'sentinel']),
        ],
        maxTurns: 30,
      } as any);
      const sentinel = directLlmRunner({
        id: 'sentinel',
        name: 'Sentinel',
        description: 'Keeps the Sentinel page current with the refusals: counts, by rule, and the latest rows.',
        adapter,
        schema: (s: any) => s.undefined(),
        instructions: () => SENTINEL,
        tools: () => [
          studio.viewSchema(),
          studio.viewReport(),
          studio.viewPage(),
          studio.executeQuery(),
          tools.renameThread(),
          tools.delegateTo(['page', 'widget']),
        ],
        maxTurns: 30,
      } as any);
      return {
        agents: [
          ...Object.values(builtIn).map((d: any) => directLlmRunner({ ...d, adapter })),
          auditor,
          sentinel,
        ],
        primary: 'auditor',
        // ?debug on the URL logs every harness event to the browser console.
        observers: new URLSearchParams(location.search).has('debug')
          ? [{ onEvent: (e: unknown) => console.debug('[desk-ai]', JSON.stringify(e).slice(0, 2000)) }]
          : undefined,
        promptStarters: STARTERS,
        models: models.length > 1 ? models.map((id) => ({ id, label: id.split('/').pop() ?? id })) : undefined,
      };
    });
    onHarness(harness);
    return harness;
  };
}
