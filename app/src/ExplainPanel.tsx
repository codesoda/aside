import { useEffect, useRef, useState } from 'react';
import { ApiError } from './api';
import { askExplanation, cancelExplanation, explanationProviders, sameSelection, type ExplainProvider, type ExplainTarget, type ExplainTurn } from './explain';
import { Button } from './components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './components/ui/select';
import { Textarea } from './components/ui/textarea';

export type ExplainPanelProps = { target: ExplainTarget; stale: boolean; onClose: () => void };
const suggestions = ['What does this red highlight mean?', 'What changed here?', 'What could this affect?', 'Is there evidence of a problem?', 'What should I check before marking this as reviewed?'];
export default function ExplainPanel({ target, stale, onClose }: ExplainPanelProps) {
  const [providers, setProviders] = useState<ExplainProvider[]>([]);
  const [providerId, setProviderId] = useState('');
  const [loadingOptions, setLoadingOptions] = useState(true);
  const [history, setHistory] = useState<ExplainTurn[]>([]);
  const [question, setQuestion] = useState('');
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [invalidated, setInvalidated] = useState(false);
  const heading = useRef<HTMLHeadingElement>(null);
  const controller = useRef<AbortController | null>(null);
  const requestId = useRef<string | null>(null);
  const staleRef = useRef(stale);
  const invalidRef = useRef(false);
  const selected = providers.find(provider => provider.id === providerId);
  function cancel(showNotice = true) {
    const id = requestId.current;
    requestId.current = null;
    controller.current?.abort(); controller.current = null;
    setPending(false);
    if (id) {
      void cancelExplanation(id).catch(() => setNotice('Stopped displaying this request. Server cancellation could not be confirmed; its 90-second timeout still applies.'));
      if (showNotice) setNotice('Cancelled. No answer was added. A hosted request may already have incurred usage.');
    }
  }
  useEffect(() => {
    const options = new AbortController();
    heading.current?.focus({ preventScroll: true });
    void explanationProviders(options.signal).then(result => {
      if (options.signal.aborted) return;
      setProviders(result.providers);
      setProviderId(result.providers.find(provider => provider.available)?.id ?? '');
    }).catch(failure => { if (!options.signal.aborted) setError(String(failure)); })
      .finally(() => { if (!options.signal.aborted) setLoadingOptions(false); });
    return () => { options.abort(); cancel(false); };
  }, []);
  staleRef.current = stale;
  useEffect(() => { if (stale) { invalidRef.current = true; setInvalidated(true); cancel(false); setHistory([]); } }, [stale]);
  function changeProvider(id: string) {
    cancel(false); setProviderId(id); setHistory([]); setError('');
    setNotice('Provider changed. A new conversation will start only when you send a question.');
  }
  async function send(text: string) {
    if (requestId.current || staleRef.current || invalidRef.current || !selected?.available || !text.trim()) return;
    const id = crypto.randomUUID(), provider = providerId, selection = target.selection;
    const abort = new AbortController(); controller.current = abort; requestId.current = id;
    setPending(true); setError(''); setNotice('');
    const timeout = setTimeout(() => { if (requestId.current === id) { cancel(false); setError('Request timed out. You can retry; no fallback was used.'); } }, 95_000);
    try {
      const reply = await askExplanation(id, provider, selection, text.trim(), history, abort.signal);
      if (requestId.current !== id || abort.signal.aborted || staleRef.current || invalidRef.current) return;
      if (reply.requestId !== id || reply.provider !== provider || !sameSelection(reply.selection, selection)) throw new Error('Response did not match this selection. Please select the change again.');
      setHistory(previous => [...previous, { question: text.trim(), answer: reply.answer }]); setQuestion('');
    } catch (failure) {
      if (requestId.current !== id || abort.signal.aborted) return;
      if (failure instanceof ApiError && failure.code === 'stale_selection') { invalidRef.current = true; setInvalidated(true); setHistory([]); }
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally { clearTimeout(timeout); if (requestId.current === id) { requestId.current = null; controller.current = null; setPending(false); } }
  }
  const noneReady = !loadingOptions && !providers.some(provider => provider.available);
  return <div className="explain-panel workspace-panel" role="dialog" aria-modal="false" tabIndex={-1} aria-label="Explain this change" onKeyDown={event => { if (event.key === 'Escape') { event.stopPropagation(); onClose(); } }}>
    <div className="workspace-panel-heading flex items-center justify-between gap-2"><h3 ref={heading} tabIndex={-1} className="font-semibold">Explain this change</h3><Button type="button" variant="outline" onClick={onClose} aria-label="Close explanation">Close</Button></div>
    <div className="space-y-3 p-4 text-sm"><p className="wrap-anywhere"><strong>{target.path}</strong><br />{target.label} · revision <code>{target.selection.headSha.slice(0, 12)}</code></p>
      <p className="text-muted">− Red: code removed from the old version. + Green: code added to the new version. These are changes, not errors. Warnings and diagnostics are separate evidence.</p>
      {stale || invalidated ? <p role="alert">The review, file or revision changed. This conversation is no longer current. Select a change again to start a new explanation.</p> : <>
        <label className="block font-semibold" htmlFor="explain-provider">Model or tool</label>
        <Select value={providerId} onValueChange={changeProvider} disabled={loadingOptions || pending}><SelectTrigger id="explain-provider" className="w-full"><SelectValue placeholder={loadingOptions ? 'Checking configured options…' : 'Choose a configured provider'} /></SelectTrigger><SelectContent>{providers.map(provider => <SelectItem key={provider.id} value={provider.id} disabled={!provider.available}>{provider.label}{provider.model ? ` · ${provider.model}` : ''} · {provider.processing}{provider.available ? '' : ' · unavailable'}</SelectItem>)}</SelectContent></Select>
        {selected && <p><strong>{selected.processing === 'local' ? 'Local processing' : 'Hosted processing'}</strong> · {selected.detail}</p>}
        {noneReady && <p>No provider is ready. Configure a local model or enable Claude using the setup instructions below, then restart Aside.</p>}
        <details open={noneReady}><summary>Provider setup and availability</summary>{providers.map(provider => <p key={provider.id} className="mt-2"><strong>{provider.label}:</strong> {provider.detail}</p>)}</details>
        <p className="text-muted">Sending shares only this change block, up to three surrounding patch lines, matching diagnostics and this conversation. No other files are read. Answers are not a safety verdict and never mark a file reviewed.</p>
        {!history.length ? <Button type="button" disabled={pending || !selected?.available} onClick={() => void send('Explain this change in plain English.')}>Explain this change</Button> : <div className="space-y-3" aria-label="Explanation conversation">{history.map((turn, index) => <article key={index} className="border-t border-line pt-2"><h4 className="font-semibold">You: {turn.question}</h4><p className="mt-2 whitespace-pre-wrap wrap-anywhere" data-explanation-answer>{turn.answer}</p></article>)}</div>}
        <form onSubmit={event => { event.preventDefault(); void send(question); }}><label htmlFor="explain-question" className="block font-semibold">Ask a follow-up question</label><Textarea id="explain-question" className="mt-1 w-full" value={question} onChange={event => setQuestion(event.target.value)} rows={2} maxLength={2000} disabled={pending || history.length > 8} placeholder="What would you like to understand?" /><Button type="submit" variant="outline" className="mt-2" disabled={pending || !selected?.available || !question.trim() || history.length > 8}>Send question</Button></form>
        <details><summary>Suggested questions</summary><ul className="mt-2 space-y-2">{suggestions.map(suggestion => <li key={suggestion}><Button type="button" variant="outline" className="text-left" disabled={pending || history.length > 8} onClick={() => setQuestion(suggestion)}>{suggestion}</Button></li>)}</ul></details>
        {!!history.length && <Button type="button" variant="outline" disabled={pending} onClick={() => { setHistory([]); setError(''); setNotice('Conversation cleared. Nothing has been sent.'); }}>New conversation</Button>}
      </>}
      {pending && <><p role="status">Preparing an explanation…</p><Button type="button" variant="outline" onClick={() => cancel()}>Cancel request</Button></>}
      {error && <p role="alert" className="text-danger">{error}</p>}
      <p role="status" className="text-muted">{notice || (history.length ? 'Answer ready. Follow-up questions use the same selected change.' : '')}</p>
      <p className="text-xs text-muted">Conversation stays in this panel only; closing it clears the local conversation. Your provider may retain requests under its own policies.</p>
    </div>
  </div>;
}
