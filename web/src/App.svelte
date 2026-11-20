<script lang="ts">
  import { fetchSession, type SessionSnapshot } from './api';

  let session = $state<SessionSnapshot | null>(null);
  let error = $state<string | null>(null);

  fetchSession()
    .then((s) => (session = s))
    .catch((e) => (error = String(e)));

  const totals = $derived(
    session?.files.reduce(
      (acc, f) => ({ add: acc.add + f.additions, del: acc.del + f.deletions }),
      { add: 0, del: 0 },
    ) ?? { add: 0, del: 0 },
  );
</script>

<div class="min-h-screen bg-white text-[#1f2328] font-sans">
  <header class="border-b border-[#d1d9e0] bg-[#f6f8fa] px-6 py-3 flex items-center gap-3">
    <span class="font-semibold">Aside</span>
    {#if session?.demo}
      <span class="rounded-full border border-[#d1d9e0] px-2 text-xs text-[#59636e]">demo data</span>
    {/if}
  </header>

  {#if error}
    <p class="p-6 text-red-700">{error}</p>
  {:else if !session}
    <p class="p-6 text-[#59636e]">Loading…</p>
  {:else}
    <main class="mx-auto max-w-6xl p-6">
      <h1 class="text-2xl">
        {session.pr.title}
        <span class="text-[#59636e] font-light">#{session.pr.number}</span>
      </h1>
      <p class="mt-1 text-sm text-[#59636e]">
        {session.pr.author} wants to merge into
        <code class="rounded bg-[#ddf4ff] px-1 text-[#0969da]">{session.pr.baseRef}</code>
        from
        <code class="rounded bg-[#ddf4ff] px-1 text-[#0969da]">{session.pr.headRef}</code>
        · {session.pr.owner}/{session.pr.repo}
      </p>

      <section class="mt-6 rounded-md border border-[#d1d9e0]">
        <div class="flex items-center justify-between border-b border-[#d1d9e0] bg-[#f6f8fa] px-4 py-2 text-sm">
          <span class="font-semibold">Pre-review · {session.files.length} changed files</span>
          <span>
            <span class="text-[#1a7f37]">+{totals.add}</span>
            <span class="text-[#d1242f]">−{totals.del}</span>
          </span>
        </div>
        <ul>
          {#each session.files as file (file.id)}
            <li class="flex items-center justify-between border-b border-[#d1d9e0] px-4 py-2 font-mono text-sm last:border-b-0">
              <span>{file.path}</span>
              <span>
                <span class="text-[#1a7f37]">+{file.additions}</span>
                <span class="text-[#d1242f]">−{file.deletions}</span>
              </span>
            </li>
          {/each}
        </ul>
      </section>

      <button
        class="mt-4 rounded-md bg-[#1f883d] px-4 py-1.5 text-sm font-medium text-white hover:bg-[#1a7f37] disabled:opacity-50"
        disabled
        title="Not built yet"
      >
        Start review
      </button>
    </main>
  {/if}
</div>
