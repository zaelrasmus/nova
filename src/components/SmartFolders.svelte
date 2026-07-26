<script lang="ts">
    import {
        Sparkles,
        Plus,
        Pencil,
        Trash2,
        FolderPlus,
        Layers2,
        Pin,
        PinOff,
    } from "@lucide/svelte";
    import { toast } from "svelte-sonner";
    import {
        assetLibrary,
        type SmartFolder,
        type SmartFolderGroup,
    } from "$lib/assets.svelte";
    import { describeRules } from "$lib/rules";
    import { selection } from "$lib/selection.svelte";
    import SmartFolderDialog from "./rules/SmartFolderDialog.svelte";

    /**
     * The smart folders section of the sidebar.
     *
     * Rendered as places, not as saved queries: clicking one NAVIGATES (it
     * becomes the scope), where clicking a saved filter narrows where you
     * already are. Same rows underneath — the difference the user sees is the
     * whole reason both exist.
     *
     * A GROUP is a place too. Clicking its header browses the union of its
     * members, which is why it gets the same active treatment as a folder
     * rather than being a mere disclosure triangle.
     */
    const folders = $derived(assetLibrary.smartFolders);
    const groups = $derived(assetLibrary.smartFolderGroups);
    const scope = $derived(assetLibrary.scope);

    const ungrouped = $derived(folders.filter((f) => f.group_id === null));
    const membersOf = (groupId: string) => folders.filter((f) => f.group_id === groupId);

    /** Open with a folder to edit it, `null` to create a new one. */
    let editing = $state<SmartFolder | null | undefined>(undefined);

    function go(next: { kind: "smart" | "smart_group"; id: string }) {
        selection.clear();
        void assetLibrary.setScope(next);
    }

    const fail = (e: unknown, fallback: string) =>
        toast.error(typeof e === "string" ? e : fallback);

    async function remove(folder: SmartFolder) {
        const ok = window.confirm(
            `Delete "${folder.name}"? Your assets aren't affected — a smart folder only collects them.`,
        );
        if (!ok) return;
        try {
            await assetLibrary.deleteSmartFolder(folder.id);
        } catch (e) {
            fail(e, "Couldn't delete the smart folder.");
        }
    }

    async function newGroup() {
        const name = window.prompt("Group name:");
        if (!name) return;
        try {
            await assetLibrary.createSmartFolderGroup(name);
        } catch (e) {
            fail(e, "Couldn't create the group.");
        }
    }

    async function removeGroup(group: SmartFolderGroup) {
        const ok = window.confirm(
            `Delete the group "${group.name}"? Its smart folders are kept — they just leave the group.`,
        );
        if (!ok) return;
        try {
            await assetLibrary.deleteSmartFolderGroup(group.id);
        } catch (e) {
            fail(e, "Couldn't delete the group.");
        }
    }

    /** Move a folder between groups. The `<select>` is the whole affordance. */
    function reassign(folder: SmartFolder, value: string) {
        void assetLibrary
            .setSmartFolderGroup(folder.id, value === "" ? null : value)
            .catch((e) => fail(e, "Couldn't move the smart folder."));
    }
</script>

{#snippet folderRow(folder: SmartFolder)}
    {@const active = scope.kind === "smart" && scope.id === folder.id}
    <div class="group flex items-center rounded {active ? 'bg-neutral-800' : ''}">
        <button
            type="button"
            onclick={() => go({ kind: "smart", id: folder.id })}
            title={describeRules(folder.rules, assetLibrary.folderNames)}
            aria-current={active ? "true" : undefined}
            class="flex min-w-0 flex-1 items-center gap-1.5 rounded px-2 py-1 text-left text-sm
                   transition-colors
                   {active ? 'text-neutral-100' : 'text-neutral-300 hover:bg-neutral-800'}"
        >
            <Sparkles class="h-3.5 w-3.5 shrink-0 text-neutral-500" strokeWidth={1.5} />
            <span class="truncate">{folder.name}</span>
        </button>
        <div class="flex shrink-0 items-center opacity-0 transition-opacity group-hover:opacity-100">
            {#if groups.length > 0}
                <select
                    value={folder.group_id ?? ""}
                    onchange={(e) => reassign(folder, e.currentTarget.value)}
                    title="Move to a group"
                    aria-label="Move to a group"
                    class="mr-1 max-w-20 rounded border border-neutral-800 bg-neutral-950 px-1
                           py-0.5 text-[10px] text-neutral-400 focus:outline-none"
                >
                    <option value="">No group</option>
                    {#each groups as g (g.id)}
                        <option value={g.id}>{g.name}</option>
                    {/each}
                </select>
            {/if}
            <button
                type="button"
                title={folder.pin_position === null ? "Pin to sidebar" : "Unpin"}
                aria-label={folder.pin_position === null ? "Pin to sidebar" : "Unpin"}
                onclick={() =>
                    assetLibrary
                        .setPinned("smart", folder.id, folder.pin_position === null)
                        .catch((e) => fail(e, "Couldn't pin the smart folder."))}
                class="px-1 {folder.pin_position === null
                    ? 'text-neutral-500 hover:text-neutral-200'
                    : 'text-blue-400'}"
            >
                {#if folder.pin_position === null}
                    <Pin class="h-3 w-3" />
                {:else}
                    <PinOff class="h-3 w-3" />
                {/if}
            </button>
            <button
                type="button"
                title="Edit rules"
                aria-label="Edit rules"
                onclick={() => (editing = folder)}
                class="px-1 text-neutral-500 hover:text-neutral-200"
            >
                <Pencil class="h-3 w-3" />
            </button>
            <button
                type="button"
                title="Delete"
                aria-label="Delete"
                onclick={() => remove(folder)}
                class="px-1 text-neutral-500 hover:text-red-400"
            >
                <Trash2 class="h-3 w-3" />
            </button>
        </div>
    </div>
{/snippet}

<div class="flex flex-col gap-0.5">
    {#each groups as group (group.id)}
        {@const active = scope.kind === "smart_group" && scope.id === group.id}
        {@const members = membersOf(group.id)}
        <div class="group flex items-center rounded {active ? 'bg-neutral-800' : ''}">
            <button
                type="button"
                onclick={() => go({ kind: "smart_group", id: group.id })}
                title="Everything in {group.name}"
                aria-current={active ? "true" : undefined}
                class="flex min-w-0 flex-1 items-center gap-1.5 rounded px-2 py-1 text-left
                       text-[11px] font-semibold uppercase tracking-wide transition-colors
                       {active ? 'text-neutral-100' : 'text-neutral-500 hover:bg-neutral-800'}"
            >
                <Layers2 class="h-3 w-3 shrink-0" strokeWidth={1.5} />
                <span class="truncate">{group.name}</span>
                <span class="shrink-0 font-normal normal-case text-neutral-600">
                    {members.length}
                </span>
            </button>
            <button
                type="button"
                title="Delete group"
                aria-label="Delete group"
                onclick={() => removeGroup(group)}
                class="px-1 text-neutral-500 opacity-0 transition-opacity hover:text-red-400
                       group-hover:opacity-100"
            >
                <Trash2 class="h-3 w-3" />
            </button>
        </div>

        <div class="ml-2 flex flex-col gap-0.5 border-l border-neutral-800 pl-1">
            {#each members as folder (folder.id)}
                {@render folderRow(folder)}
            {:else}
                <p class="px-2 py-0.5 text-[11px] text-neutral-600">Empty group.</p>
            {/each}
        </div>
    {/each}

    {#each ungrouped as folder (folder.id)}
        {@render folderRow(folder)}
    {/each}

    <div class="mt-1 flex items-center gap-1">
        <button
            type="button"
            onclick={() => (editing = null)}
            class="flex flex-1 items-center gap-1.5 rounded px-2 py-1 text-left text-xs
                   text-neutral-500 transition-colors hover:bg-neutral-800 hover:text-neutral-300"
        >
            <Plus class="h-3 w-3" /> New smart folder
        </button>
        <button
            type="button"
            onclick={newGroup}
            title="New group"
            aria-label="New group"
            class="rounded px-1.5 py-1 text-neutral-500 transition-colors hover:bg-neutral-800
                   hover:text-neutral-300"
        >
            <FolderPlus class="h-3 w-3" />
        </button>
    </div>
</div>

{#if editing !== undefined}
    <SmartFolderDialog existing={editing ?? undefined} onclose={() => (editing = undefined)} />
{/if}
