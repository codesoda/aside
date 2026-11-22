<script lang="ts">
  import { onMount, onDestroy, untrack } from 'svelte';
  import { ApiError } from './api';
  import { askExplanation, cancelExplanation, explanationProviders, sameSelection,
    type ExplainProvider, type ExplainTarget, type ExplainTurn } from './explain';

  let { target, stale, onClose }: { target: ExplainTarget; stale: boolean; onClose: () => void } = $props();
  let providers = $state<ExplainProvider[]>([]);
  let providerId = $state('');
  let loadingOptions = $state(true);
  let history = $state<ExplainTurn[]>([]);
  let question = $state('');
  let pending = $state(false);
  let error = $state('');
  let notice = $state('');
  let invalidated = $state(false);
  let heading: HTMLHeadingElement;
  let controller: AbortController | null = null;
  let requestId: string | null = null;
  const optionsController = new AbortController();
  const selected = $derived(providers.find((p) => p.id === providerId));
  const suggestions = [
    'What does this red highlight mean?', 'What changed here?', 'What could this affect?',
    'Is there evidence of a problem?', 'What should I check before marking this as reviewed?',
  ];

  onMount(() => {
    heading?.focus({ preventScroll: true });
    void explanationProviders(optionsController.signal).then((result) => {
      providers = result.providers;
      // Prefer local; never switch provider or send a question automatically.
      providerId = providers.find((p) => p.available)?.id ?? '';
    }).catch((failure) => {
      if (!optionsController.signal.aborted) error = String(failure);
    }).finally(() => { loadingOptions = false; });
  });
  onDestroy(() => { optionsController.abort(); cancel(false); });

  $effect(() => {
    if (stale) untrack(() => { invalidated = true; cancel(false); history = []; });
  });

  function cancel(showNotice = true) {
    const id = requestId;
    requestId = null;
    controller?.abort();
    controller = null;
    pending = false;
    if (id) {
      void cancelExplanation(id).catch(() => {
        notice = 'Stopped displaying this request. Server cancellation could not be confirmed; its 90-second timeout still applies.';
      });
      if (showNotice) notice = 'Cancelled. No answer was added. A hosted request may already have incurred usage.';
    }
  }

  function changeProvider() {
    cancel(false);
    history = [];
    error = '';
    notice = 'Provider changed. A new conversation will start only when you send a question.';
  }

  async function send(text: string) {
    if (pending || stale || invalidated || !selected?.available || !text.trim()) return;
    const id = crypto.randomUUID();
    const provider = providerId;
    const selection = target.selection;
    const abort = new AbortController();
    controller = abort;
    requestId = id;
    pending = true;
    error = '';
    notice = '';
    // Client timeout also stops a disconnected HTTP request. Server has its own deadline.
    const timeout = setTimeout(() => {
      if (requestId === id) { cancel(false); error = 'Request timed out. You can retry; no fallback was used.'; }
    }, 95_000);
    try {
      const reply = await askExplanation(id, provider, selection, text.trim(), history, abort.signal);
      if (requestId !== id || abort.signal.aborted || stale || invalidated) return;
      if (reply.requestId !== id || reply.provider !== provider || !sameSelection(reply.selection, selection)) {
        throw new Error('Response did not match this selection. Please select the change again.');
      }
      history = [...history, { question: text.trim(), answer: reply.answer }];
      question = '';
    } catch (failure) {
      if (requestId !== id || abort.signal.aborted) return;
      if (failure instanceof ApiError && failure.code === 'stale_selection') { invalidated = true; history = []; }
      error = failure instanceof Error ? failure.message : String(failure);
    } finally {
      clearTimeout(timeout);
      if (requestId === id) { requestId = null; controller = null; pending = false; }
    }
  }
</script>

<div class="explain-panel workspace-panel" role="dialog" aria-modal="false" tabindex="-1" aria-label="Explain this change" onkeydown={(event) => {
  if (event.key === 'Escape') { event.stopPropagation(); onClose(); }
}}>
  <div class="workspace-panel-heading flex items-center justify-between gap-2">
    <h3 bind:this={heading} tabindex="-1" class="font-semibold">Explain this change</h3>
    <button type="button" class="workspace-button" onclick={onClose} aria-label="Close explanation">Close</button>
  </div>
  <div class="space-y-3 p-4 text-sm">
    <p class="wrap-anywhere"><strong>{target.path}</strong><br />{target.label} · revision <code>{target.selection.headSha.slice(0, 12)}</code></p>
    <p class="text-muted">− Red: code removed from the old version. + Green: code added to the new version. These are changes, not errors. Warnings and diagnostics are separate evidence.</p>
    {#if stale || invalidated}
      <p role="alert">The review, file or revision changed. This conversation is no longer current. Select a change again to start a new explanation.</p>
    {:else}
      <label class="block font-semibold" for="explain-provider">Model or tool</label>
      <select id="explain-provider" class="workspace-input w-full" bind:value={providerId} onchange={changeProvider} disabled={loadingOptions || pending}>
        <option value="" disabled>{loadingOptions ? 'Checking configured options…' : 'Choose a configured provider'}</option>
        {#each providers as provider (provider.id)}
          <option value={provider.id} disabled={!provider.available}>{provider.label}{provider.model ? ` · ${provider.model}` : ''} · {provider.processing}{provider.available ? '' : ' · unavailable'}</option>
        {/each}
      </select>
      {#if selected}<p><strong>{selected.processing === 'local' ? 'Local processing' : 'Hosted processing'}</strong> · {selected.detail}</p>{/if}
      {#if !loadingOptions && !providers.some((provider) => provider.available)}
        <p>No provider is ready. Configure a local model or enable Claude using the setup instructions below, then restart Aside.</p>
      {/if}
      <details open={!loadingOptions && !providers.some((provider) => provider.available)}>
        <summary>Provider setup and availability</summary>
        {#each providers as provider (provider.id)}<p class="mt-2"><strong>{provider.label}:</strong> {provider.detail}</p>{/each}
      </details>
      <p class="text-muted">Sending shares only this change block, up to three surrounding patch lines, matching diagnostics and this conversation. No other files are read. Answers are not a safety verdict and never mark a file reviewed.</p>
      {#if history.length === 0}
        <button type="button" class="workspace-button primary-button" disabled={pending || !selected?.available} onclick={() => void send('Explain this change in plain English.')}>Explain this change</button>
      {:else}
        <div class="space-y-3" aria-label="Explanation conversation">
          {#each history as turn}
            <article class="border-t border-line pt-2">
              <h4 class="font-semibold">You: {turn.question}</h4>
              <!-- Svelte text interpolation escapes provider output. No HTML or Markdown execution. -->
              <p class="mt-2 whitespace-pre-wrap wrap-anywhere" data-explanation-answer>{turn.answer}</p>
            </article>
          {/each}
        </div>
      {/if}
      <form onsubmit={(event) => { event.preventDefault(); void send(question); }}>
        <label for="explain-question" class="block font-semibold">Ask a follow-up question</label>
        <textarea id="explain-question" class="workspace-input mt-1 w-full" bind:value={question} rows="2" maxlength="2000" disabled={pending || history.length > 8} placeholder="What would you like to understand?"></textarea>
        <button type="submit" class="workspace-button mt-2" disabled={pending || !selected?.available || !question.trim() || history.length > 8}>Send question</button>
      </form>
      <details>
        <summary>Suggested questions</summary>
        <ul class="mt-2 space-y-2">
          {#each suggestions as suggestion}<li><button type="button" class="workspace-button text-left" disabled={pending || history.length > 8} onclick={() => { question = suggestion; }}>{suggestion}</button></li>{/each}
        </ul>
      </details>
      {#if history.length}<button type="button" class="workspace-button" disabled={pending} onclick={() => { history = []; error = ''; notice = 'Conversation cleared. Nothing has been sent.'; }}>New conversation</button>{/if}
    {/if}
    {#if pending}<p role="status">Preparing an explanation…</p><button type="button" class="workspace-button" onclick={() => cancel()}>Cancel request</button>{/if}
    {#if error}<p role="alert" class="text-danger">{error}</p>{/if}
    <p role="status" class="text-muted">{notice || (history.length ? 'Answer ready. Follow-up questions use the same selected change.' : '')}</p>
    <p class="text-xs text-muted">Conversation stays in this panel only; closing it clears the local conversation. Your provider may retain requests under its own policies.</p>
  </div>
</div>
