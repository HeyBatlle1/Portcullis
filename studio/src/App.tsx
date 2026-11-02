import { useEffect, useMemo, useRef, useState } from 'react';
import { AgStudioAiModule, type AgAiHarness, type AgStudioMode } from 'ag-studio';
import { AgStudio, AgStudioProvider } from 'ag-studio-react';
import { buildData } from './data';
import { onRecord, start, useFeed } from './feed';
import { initialReport } from './report';
import { deskTheme } from './theme';
import { widgets } from './widgets';
import { buildAi } from './ai/harness';

interface Config {
  licenseKey: string | null;
  ai: boolean;
  models: string[];
}

/** Refusals that arrive within this long of each other are one event to the sentinel. */
const SETTLE_MS = 6000;

export function App() {
  const [config, setConfig] = useState<Config | null>(null);
  useEffect(() => {
    start();
    fetch('/audit/config')
      .then((r) => r.json())
      .then(setConfig)
      .catch(() => setConfig({ licenseKey: null, ai: false, models: [] }));
  }, []);
  if (!config) return <div className="boot">Opening the audit desk…</div>;
  return (
    <AgStudioProvider licenseKey={config.licenseKey ?? undefined} modules={config.ai ? [AgStudioAiModule] : undefined}>
      <Desk config={config} />
    </AgStudioProvider>
  );
}

function Desk({ config }: { config: Config }) {
  const feed = useFeed();
  const [mode, setMode] = useState<AgStudioMode>('view');
  const [watching, setWatching] = useState(false);
  const [sentinelNote, setSentinelNote] = useState('');
  const harness = useRef<AgAiHarness | null>(null);

  // Rebuild Studio's tables when the chain grows. The records are the only input.
  const data = useMemo(() => buildData(feed.records), [feed.records]);
  const ai = useMemo(
    () => (config.ai ? buildAi(config.models, (h) => (harness.current = h)) : undefined),
    [config],
  );

  // The sentinel: off until a person turns it on, then started by the record itself.
  useEffect(() => {
    if (!watching) return;
    let pending: { n: number; rules: Set<string> } | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const off = onRecord((r) => {
      if (r.kind !== 'decision') return;
      const b = JSON.parse(r.body);
      const refuse = b.adjudication?.decision?.Refuse;
      if (!refuse) return;
      pending ??= { n: 0, rules: new Set() };
      pending.n++;
      pending.rules.add(refuse.rule);
      clearTimeout(timer);
      timer = setTimeout(async () => {
        const p = pending!;
        pending = null;
        const h = harness.current;
        if (!h) return;
        setSentinelNote(`Sentinel is updating its page: ${p.n} new refusal${p.n > 1 ? 's' : ''}.`);
        const session = await h.createThread({ agentId: 'sentinel', title: 'Sentinel: new refusals' });
        session.sendMessage(
          `${p.n} new refusal${p.n > 1 ? 's' : ''} just arrived (${[...p.rules].join(', ')}). Update the Sentinel page.`,
        );
      }, SETTLE_MS);
    });
    return () => {
      off();
      clearTimeout(timer);
    };
  }, [watching]);

  return (
    <div className="desk">
      <header className="bar">
        <a className="mark" href="/console">
          <svg viewBox="0 0 26 30" aria-hidden="true">
            <path d="M2 3h22M2 3v25M24 3v25M7 3v27M13 3v27M19 3v27M2 11h22M2 19h22" />
          </svg>
          <b>Portcullis</b>
        </a>
        <span className="where">Audit desk</span>
        <p>Every decision the gate made, from the hash-chained record, verified in this browser.</p>
        <div className="controls">
          {config.ai && (
            <label className="toggle">
              <input type="checkbox" checked={watching} onChange={(e) => setWatching(e.target.checked)} />
              Sentinel watches for refusals
            </label>
          )}
          <button onClick={() => setMode(mode === 'view' ? 'edit' : 'view')}>
            {mode === 'view' ? 'Edit and ask the agents' : 'Done editing'}
          </button>
          <a href="/console">Back to the console</a>
        </div>
        {sentinelNote && <span className="note">{sentinelNote}</span>}
        {!feed.connected && <span className="note warn">Reconnecting to the gate…</span>}
      </header>
      <main>
        <AgStudio
          style={{ height: '100%', width: '100%' }}
          theme={deskTheme}
          widgets={widgets as any}
          layout={{ widgetBorderEnabled: true } as any}
          data={data}
          ai={ai as any}
          initialState={initialReport}
          mode={mode}
        />
      </main>
    </div>
  );
}
