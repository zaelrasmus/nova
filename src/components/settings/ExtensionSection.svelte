<!--
  Settings › Browser extension.

  This panel is the authentication. The bridge is an HTTP server on loopback, so
  a pairing request can arrive from anything that can open a socket — what makes
  it safe is that a token is only ever minted after a human approves it *here*,
  in a window the network cannot reach.

  Which is why the ORIGIN is shown as prominently as the name. The name is
  whatever the caller typed; the origin is the thing actually being pinned, and
  the only way to tell the extension you installed from something else asking at
  the same moment.
-->
<script lang="ts">
    import { onMount } from "svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { listen, type UnlistenFn } from "@tauri-apps/api/event";
    import { toast } from "svelte-sonner";
    import { Check, X, Plug, TriangleAlert } from "@lucide/svelte";
    import { formatTimestamp } from "$lib/format";

    interface ClientSummary {
        id: string;
        name: string;
        origin: string;
        paired_at: string;
    }
    interface PendingSummary {
        id: string;
        name: string;
        origin: string;
    }
    interface BridgeSnapshot {
        /** null when every candidate port was taken — the bridge is not listening. */
        port: number | null;
        clients: ClientSummary[];
        pending: PendingSummary[];
        idle_exit_secs: number;
    }

    /**
     * How long Nova stays in the background before exiting.
     *
     * Discrete choices rather than a free number: the meaningful difference is
     * between "leave immediately", "stick around for a burst of saves" and
     * "stay put", and a spinner inviting 437 seconds would imply a precision
     * that does not exist. The 1-minute option is also the only practical way to
     * watch the behaviour — fifteen minutes is a long time to sit and confirm
     * that something disappeared.
     */
    const IDLE_CHOICES: { secs: number; label: string; hint?: string }[] = [
        { secs: 60, label: "1 min", hint: "Leaves almost immediately" },
        { secs: 5 * 60, label: "5 min" },
        { secs: 15 * 60, label: "15 min", hint: "Default" },
        { secs: 60 * 60, label: "1 hour" },
        { secs: 24 * 60 * 60, label: "Stay open", hint: "Never exits on its own" },
    ];

    let snapshot = $state<BridgeSnapshot | null>(null);
    let busy = $state<string | null>(null);

    async function refresh() {
        try {
            snapshot = await invoke<BridgeSnapshot>("bridge_state");
        } catch (e) {
            console.error("Could not read bridge state:", e);
        }
    }

    // The pairing request is parked on a socket with a two-minute timeout, so
    // this panel has to react the moment one arrives rather than on next open.
    onMount(() => {
        void refresh();
        let unlisten: UnlistenFn | null = null;
        void listen("bridge-changed", () => void refresh()).then((fn) => (unlisten = fn));
        return () => unlisten?.();
    });

    async function approve(id: string) {
        busy = id;
        try {
            await invoke("bridge_approve", { requestId: id });
            toast.success("Extension paired");
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't approve that request.");
        } finally {
            busy = null;
            void refresh();
        }
    }

    async function deny(id: string) {
        busy = id;
        try {
            await invoke("bridge_deny", { requestId: id });
        } catch (e) {
            console.error(e);
        } finally {
            busy = null;
            void refresh();
        }
    }

    async function setIdle(seconds: number) {
        busy = "idle";
        try {
            await invoke("bridge_set_idle_exit", { seconds });
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't change that setting.");
        } finally {
            busy = null;
            void refresh();
        }
    }

    async function revoke(client: ClientSummary) {
        const ok = window.confirm(
            `Disconnect ${client.name}? It will stop being able to save to this library until you pair it again.`,
        );
        if (!ok) return;
        busy = client.id;
        try {
            await invoke("bridge_revoke", { clientId: client.id });
            toast.success(`Disconnected ${client.name}`);
        } catch (e) {
            toast.error(typeof e === "string" ? e : "Couldn't disconnect that browser.");
        } finally {
            busy = null;
            void refresh();
        }
    }
</script>

<div class="flex flex-col gap-6">
    <!-- Status -->
    <div class="flex items-start gap-3">
        <div
            class="mt-0.5 grid h-8 w-8 shrink-0 place-items-center rounded-lg
                   {snapshot?.port ? 'bg-blue-500/15 text-blue-400' : 'bg-neutral-800 text-neutral-500'}"
        >
            <Plug class="h-4 w-4" />
        </div>
        <div class="min-w-0">
            <div class="text-sm font-medium text-neutral-100">
                {#if snapshot?.port}
                    Listening on 127.0.0.1:{snapshot.port}
                {:else}
                    Not listening
                {/if}
            </div>
            <p class="mt-1 max-w-prose text-xs leading-relaxed text-neutral-500">
                The browser extension saves to this library over a local connection. It is
                reachable only from this machine, and only by a browser you have paired below.
            </p>
        </div>
    </div>

    {#if snapshot && snapshot.port === null}
        <div
            class="flex gap-3 rounded-lg border border-amber-900/50 bg-amber-950/20 p-3
                   text-xs leading-relaxed text-amber-200/90"
        >
            <TriangleAlert class="mt-0.5 h-4 w-4 shrink-0 text-amber-400/80" />
            <div>
                Nova couldn't claim a port, so the extension can't reach it. Another program is
                likely using them — restarting Nova usually clears it.
            </div>
        </div>
    {/if}

    <!-- Pending requests. Shown above the paired list because one is waiting on
         a socket with a timeout, and the other is settled. -->
    {#if snapshot?.pending.length}
        <div class="flex flex-col gap-2">
            <span class="text-xs font-medium text-neutral-300">Waiting for approval</span>
            {#each snapshot.pending as request (request.id)}
                <div
                    class="flex items-center gap-3 rounded-lg border border-blue-500/40
                           bg-blue-500/[0.07] p-3"
                >
                    <div class="min-w-0 flex-1">
                        <div class="text-sm text-neutral-100">{request.name}</div>
                        <!-- Break-all: this is a long opaque origin, and the whole
                             point is that it can be read and compared. -->
                        <div class="mt-0.5 break-all font-mono text-[11px] text-neutral-500">
                            {request.origin}
                        </div>
                    </div>
                    <button
                        type="button"
                        onclick={() => deny(request.id)}
                        disabled={busy === request.id}
                        class="grid h-7 w-7 shrink-0 place-items-center rounded-md text-neutral-400
                               transition-colors hover:bg-neutral-800 hover:text-neutral-200
                               disabled:opacity-40"
                        title="Decline"
                        aria-label="Decline"
                    >
                        <X class="h-4 w-4" />
                    </button>
                    <button
                        type="button"
                        onclick={() => approve(request.id)}
                        disabled={busy === request.id}
                        class="inline-flex shrink-0 items-center gap-1.5 rounded-md bg-blue-600
                               px-2.5 py-1.5 text-xs font-medium text-white transition-colors
                               hover:bg-blue-500 disabled:opacity-40"
                    >
                        <Check class="h-3.5 w-3.5" />
                        Allow
                    </button>
                </div>
            {/each}
        </div>
    {/if}

    <div class="h-px bg-neutral-800" aria-hidden="true"></div>

    <!-- Background lifetime -->
    <div class="flex flex-col gap-2">
        <div class="flex flex-col gap-0.5">
            <span class="text-xs font-medium text-neutral-300">Stay in the background for</span>
            <p class="max-w-prose text-[11px] leading-relaxed text-neutral-500">
                After you close the window Nova keeps running so the extension can save
                instantly, then exits on its own. While it waits it uses about 30MB and no
                browser engine at all. It never exits with work in progress, or while the
                window is open.
            </p>
        </div>
        <div class="inline-flex w-fit flex-wrap gap-1 rounded-md border border-neutral-800 p-0.5">
            {#each IDLE_CHOICES as choice (choice.secs)}
                <button
                    type="button"
                    title={choice.hint ?? ""}
                    aria-pressed={snapshot?.idle_exit_secs === choice.secs}
                    onclick={() => setIdle(choice.secs)}
                    disabled={busy === "idle"}
                    class="rounded px-2.5 py-1 text-xs font-medium transition-colors
                        disabled:opacity-40
                        {snapshot?.idle_exit_secs === choice.secs
                        ? 'bg-blue-600 text-white'
                        : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200'}"
                >
                    {choice.label}
                </button>
            {/each}
        </div>
        <p class="text-[11px] text-neutral-600">
            Takes effect when you close the window — that is when the countdown starts.
        </p>
    </div>

    <div class="h-px bg-neutral-800" aria-hidden="true"></div>

    <!-- Paired browsers -->
    <div class="flex flex-col gap-2">
        <span class="text-xs font-medium text-neutral-300">Paired browsers</span>
        {#if !snapshot?.clients.length}
            <p class="text-xs leading-relaxed text-neutral-500">
                None yet. Install the extension and use its Pair button — the request will
                appear here for you to allow.
            </p>
        {:else}
            {#each snapshot.clients as client (client.id)}
                <div
                    class="flex items-center gap-3 rounded-lg border border-neutral-800
                           bg-neutral-900/60 p-3"
                >
                    <div class="min-w-0 flex-1">
                        <div class="text-sm text-neutral-100">{client.name}</div>
                        <div class="mt-0.5 break-all font-mono text-[11px] text-neutral-600">
                            {client.origin}
                        </div>
                        <div class="mt-0.5 text-[11px] text-neutral-500">
                            Paired {formatTimestamp(client.paired_at)}
                        </div>
                    </div>
                    <button
                        type="button"
                        onclick={() => revoke(client)}
                        disabled={busy === client.id}
                        class="shrink-0 rounded-md border border-neutral-800 px-2.5 py-1.5 text-xs
                               text-neutral-300 transition-colors hover:bg-neutral-800
                               disabled:opacity-40"
                    >
                        Disconnect
                    </button>
                </div>
            {/each}
        {/if}
    </div>
</div>
