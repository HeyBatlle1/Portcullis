// The desk as it opens. Field ids refer to the tables in ./data.ts; widget types are Studio's own
// plus the three in ./widgets.tsx.

import type { AgReportState, AgWidgetLayoutState } from 'ag-studio';
import { FONT } from './theme';

const at = (xTrack: number, yTrack: number, xSpan: number, ySpan: number): AgWidgetLayoutState => ({
  xTrack,
  yTrack,
  xSpan,
  ySpan,
});
const sum = (id: string) => ({ id, aggregation: 'sum' as const });
const type = (fontSize: number, fontWeight: 'normal' | 'bold' = 'normal') => ({
  fontFamily: FONT,
  fontSize,
  fontWeight,
  fontStyle: 'normal' as const,
});
const kpi = (text: string, value: { id: string; aggregation: 'sum' }) => ({
  type: 'value' as const,
  dataMapping: { value: [value] },
  format: {
    title: { enabled: true, text, typography: type(13, 'bold') },
    caption: { enabled: true, text: '', typography: type(0) },
    style: { typography: type(26, 'bold') },
  },
});
const title = (text: string) => ({ title: { enabled: true, text } });
const grid = (cols: string[], text: string) => ({
  type: 'grid' as const,
  dataMapping: { cols: cols.map((id) => ({ id })) },
  format: title(text),
});

export const initialReport: AgReportState<any> = {
  pages: [
    {
      id: 'desk',
      title: 'Audit desk',
      widgets: {
        seal: { type: 'chain-seal', format: title('Audit chain') },
        allowed: kpi('Paid to approved payees', sum('decisions.allowedAmount')),
        refusals: kpi('Attempts refused', sum('decisions.refused')),
        kept: kpi('Value of refused requests', sum('decisions.refusedAmount')),
        stamps: {
          type: 'refusal-stamps',
          dataMapping: { rule: [{ id: 'decisions.rule' }], count: [sum('decisions.refused')] },
          format: title('What the gate refused, by rule'),
        },
        headroom: { type: 'headroom', format: title('Headroom under the policy') },
        byAgent: {
          type: 'column-chart-stacked',
          dataMapping: {
            categoryKey: [{ id: 'decisions.agent' }],
            valueKey: [sum('decisions.allowed'), sum('decisions.refused')],
          },
          format: title('Decisions by agent'),
        },
        decisions: grid(
          [
            'decisions.seq',
            'decisions.at',
            'decisions.agent',
            'decisions.outcome',
            'decisions.rule',
            'decisions.amount',
            'decisions.recipient',
            'decisions.bill',
            'decisions.purpose',
          ],
          'Every decision',
        ),
      },
      widgetLayout: {
        seal: at(0, 0, 10, 7),
        allowed: at(10, 0, 5, 7),
        refusals: at(15, 0, 4, 7),
        kept: at(19, 0, 5, 7),
        stamps: at(0, 7, 12, 15),
        headroom: at(12, 7, 12, 9),
        byAgent: at(12, 16, 12, 12),
        decisions: at(0, 28, 24, 18),
      },
    },
    {
      id: 'payouts',
      title: 'Payouts and approvals',
      widgets: {
        steps: grid(['payouts.seq', 'payouts.at', 'payouts.step', 'payouts.batch', 'payouts.reason'], 'What PayPal did'),
        approvals: grid(
          ['approvals.seq', 'approvals.at', 'approvals.action', 'approvals.bill', 'approvals.amount', 'approvals.payee'],
          'What the owner approved',
        ),
      },
      widgetLayout: {
        steps: at(0, 0, 24, 16),
        approvals: at(0, 16, 24, 14),
      },
    },
    {
      id: 'alerts',
      title: 'Sentinel',
      widgets: {
        note: {
          type: 'text',
          format: {
            style: {
              text: 'The sentinel agent keeps this page. When refusals arrive it reads the record and adds or updates widgets here on its own. Turn it on at the top of the page.',
              typography: type(14),
              color: '#52607d',
            },
          },
        },
      },
      widgetLayout: { note: at(0, 0, 24, 4) },
    },
  ],
  selectedPageId: 'desk',
  panels: { filters: { collapsed: true }, data: { collapsed: true } },
} as any;
