<script lang="ts">
    import { toast } from "svelte-sonner";
    import { assetLibrary, type Tag, type TagGroup } from "$lib/assets.svelte";

    interface Props {
        onClose: () => void;
    }
    let { onClose }: Props = $props();

    const tags = $derived(assetLibrary.tags);
    const groups = $derived(assetLibrary.tagGroups);

    // Which sidebar bucket is showing. Groups are addressed by id.
    type View = { kind: "all" | "uncategorized" | "starred" } | { kind: "group"; id: string };
    let view = $state<View>({ kind: "all" });

    const counts = $derived({
        all: tags.length,
        uncategorized: tags.filter((t) => t.group_id === null).length,
        starred: tags.filter((t) => t.is_starred).length,
    });

    // A group can be deleted while selected; fall back to All rather than showing
    // a dead view. `v` captured to a const so the union narrowing survives into
    // the nested `find` closure.
    const activeGroup = $derived.by(() => {
        const v = view;
        return v.kind === "group" ? groups.find((g) => g.id === v.id) : undefined;
    });
    $effect(() => {
        if (view.kind === "group" && !activeGroup) view = { kind: "all" };
    });

    const visible = $derived.by(() => {
        const v = view;
        // Positive check on "group" so TS narrows to the member that has `id`,
        // rather than relying on eliminating the other three.
        if (v.kind === "group") {
            const gid = v.id;
            return tags.filter((t) => t.group_id === gid);
        }
        if (v.kind === "uncategorized") return tags.filter((t) => t.group_id === null);
        if (v.kind === "starred") return tags.filter((t) => t.is_starred);
        return tags;
    });

    const title = $derived(
        view.kind === "all"
            ? "All tags"
            : view.kind === "uncategorized"
              ? "Ungrouped"
              : view.kind === "starred"
                ? "Starred"
                : (activeGroup?.name ?? ""),
    );

    let newTagName = $state("");

    // Escape closes the manager (unless focus is in a field being typed into).
    $effect(() => {
        const onKey = (e: KeyboardEvent) => {
            const t = e.target as HTMLElement | null;
            const typing = t && /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName);
            if (e.key === "Escape" && !typing) onClose();
        };
        window.addEventListener("keydown", onKey);
        return () => window.removeEventListener("keydown", onKey);
    });

    async function guard<T>(p: Promise<T>, msg: string): Promise<T | undefined> {
        try {
            return await p;
        } catch (e) {
            toast.error(typeof e === "string" ? e : msg);
        }
    }

    async function createTag() {
        const name = newTagName.trim();
        if (!name) return;
        newTagName = "";
        const id = await guard(assetLibrary.ensureTag(name), "Failed to create tag.");
        // Created from inside a group → drop it straight into that group.
        if (id && view.kind === "group") {
            await guard(assetLibrary.setTagGroup(id, view.id), "Failed to add to group.");
        }
    }

    async function renameTag(tag: Tag) {
        const name = window.prompt("Rename tag:", tag.name);
        if (!name?.trim() || name.trim() === tag.name) return;
        await guard(assetLibrary.renameTag(tag.id, name.trim()), "Failed to rename tag.");
    }

    async function deleteTag(tag: Tag) {
        const msg =
            tag.usage > 0
                ? `Delete "${tag.name}"? It will be removed from ${tag.usage} asset${tag.usage === 1 ? "" : "s"}.`
                : `Delete "${tag.name}"?`;
        if (!window.confirm(msg)) return;
        await guard(assetLibrary.deleteTag(tag.id), "Failed to delete tag.");
    }

    async function mergeInto(source: Tag, targetId: string) {
        const target = tags.find((t) => t.id === targetId);
        if (!target) return;
        const ok = window.confirm(
            `Merge "${source.name}" into "${target.name}"?\n\n` +
                `Its ${source.usage} asset${source.usage === 1 ? "" : "s"} will be reassigned to ` +
                `"${target.name}", and "${source.name}" will be deleted. This can't be undone.`,
        );
        if (!ok) return;
        await guard(assetLibrary.mergeTags(source.id, target.id), "Failed to merge tags.");
    }

    async function newGroup() {
        const name = window.prompt("Group name:");
        if (!name?.trim()) return;
        await guard(assetLibrary.createTagGroup(name.trim()), "Failed to create group.");
    }

    async function renameGroup(group: TagGroup) {
        const name = window.prompt("Rename group:", group.name);
        if (!name?.trim() || name.trim() === group.name) return;
        await guard(assetLibrary.renameTagGroup(group.id, name.trim()), "Failed to rename group.");
    }

    async function deleteGroup(group: TagGroup) {
        const ok = window.confirm(
            `Delete group "${group.name}"? Its ${group.tag_count} tag${group.tag_count === 1 ? "" : "s"} ` +
                `will be kept and become ungrouped.`,
        );
        if (!ok) return;
        await guard(assetLibrary.deleteTagGroup(group.id), "Failed to delete group.");
    }

    const swatch = (t: Tag) => t.color ?? "#9ca3af";

    const rowBtn =
        "rounded px-2 py-1 text-left text-sm transition-colors flex items-center gap-2";
</script>

<div class="fixed inset-0 z-50 flex flex-col bg-neutral-950 text-neutral-200">
    <!-- Header -->
    <div class="flex items-center justify-between border-b border-neutral-800 px-4 py-2.5">
        <h1 class="text-sm font-semibold">Tags</h1>
        <button
            type="button"
            onclick={onClose}
            class="rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
        >
            Close ✕
        </button>
    </div>

    <div class="flex min-h-0 flex-1">
        <!-- Sidebar -->
        <div class="flex w-56 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-neutral-800 p-2">
            <button
                type="button"
                onclick={() => (view = { kind: "all" })}
                class="{rowBtn} {view.kind === 'all'
                    ? 'bg-neutral-800 text-neutral-100'
                    : 'text-neutral-300 hover:bg-neutral-800/60'}"
            >
                <span class="flex-1">All</span>
                <span class="text-xs text-neutral-500">{counts.all}</span>
            </button>
            <button
                type="button"
                onclick={() => (view = { kind: "uncategorized" })}
                class="{rowBtn} {view.kind === 'uncategorized'
                    ? 'bg-neutral-800 text-neutral-100'
                    : 'text-neutral-300 hover:bg-neutral-800/60'}"
            >
                <span class="flex-1">Ungrouped</span>
                <span class="text-xs text-neutral-500">{counts.uncategorized}</span>
            </button>
            <button
                type="button"
                onclick={() => (view = { kind: "starred" })}
                class="{rowBtn} {view.kind === 'starred'
                    ? 'bg-neutral-800 text-neutral-100'
                    : 'text-neutral-300 hover:bg-neutral-800/60'}"
            >
                <span class="flex-1">★ Starred</span>
                <span class="text-xs text-neutral-500">{counts.starred}</span>
            </button>

            <div class="mt-3 flex items-center justify-between px-2 pb-1">
                <span class="text-[11px] font-semibold uppercase tracking-wide text-neutral-500">
                    Groups ({groups.length})
                </span>
                <button
                    type="button"
                    onclick={newGroup}
                    title="New group"
                    class="text-neutral-500 hover:text-neutral-200">＋</button
                >
            </div>

            {#each groups as group (group.id)}
                <div class="group flex items-center">
                    <button
                        type="button"
                        onclick={() => (view = { kind: "group", id: group.id })}
                        class="{rowBtn} flex-1 {view.kind === 'group' && view.id === group.id
                            ? 'bg-neutral-800 text-neutral-100'
                            : 'text-neutral-300 hover:bg-neutral-800/60'}"
                    >
                        <span
                            class="h-2.5 w-2.5 shrink-0 rounded-sm"
                            style="background-color: {group.color ?? '#6b7280'}"
                        ></span>
                        <span class="flex-1 truncate">{group.name}</span>
                        <span class="text-xs text-neutral-500">{group.tag_count}</span>
                    </button>
                    <div class="flex shrink-0 opacity-0 transition-opacity group-hover:opacity-100">
                        <label
                            title="Group color"
                            class="cursor-pointer px-1 text-neutral-500 hover:text-neutral-200"
                        >
                            🎨
                            <input
                                type="color"
                                value={group.color ?? "#6b7280"}
                                onchange={(e) =>
                                    assetLibrary.setTagGroupColor(group.id, e.currentTarget.value)}
                                class="sr-only"
                            />
                        </label>
                        <button
                            type="button"
                            title="Rename group"
                            onclick={() => renameGroup(group)}
                            class="px-1 text-neutral-500 hover:text-neutral-200">✎</button
                        >
                        <button
                            type="button"
                            title="Delete group"
                            onclick={() => deleteGroup(group)}
                            class="px-1 text-neutral-500 hover:text-red-400">🗑</button
                        >
                    </div>
                </div>
            {/each}
        </div>

        <!-- Main panel -->
        <div class="flex min-w-0 flex-1 flex-col">
            <div class="flex items-center gap-3 border-b border-neutral-800 px-4 py-2.5">
                <h2 class="text-sm font-medium">{title}</h2>
                <span class="text-xs text-neutral-500">({visible.length})</span>
                <div class="ml-auto flex items-center gap-2">
                    <input
                        type="text"
                        bind:value={newTagName}
                        onkeydown={(e) => e.key === "Enter" && createTag()}
                        placeholder={view.kind === "group"
                            ? `New tag in ${title}…`
                            : "New tag…"}
                        spellcheck="false"
                        class="w-48 rounded border border-neutral-700 bg-neutral-900 px-2 py-1 text-xs
                               text-neutral-200 placeholder:text-neutral-600 focus:border-neutral-500
                               focus:outline-none"
                    />
                </div>
            </div>

            <div class="min-h-0 flex-1 overflow-y-auto p-2">
                {#if visible.length === 0}
                    <p class="p-4 text-sm text-neutral-600">No tags here yet.</p>
                {:else}
                    {#each visible as tag (tag.id)}
                        <div
                            class="group flex items-center gap-2 rounded px-2 py-1.5 hover:bg-neutral-900"
                        >
                            <button
                                type="button"
                                onclick={() => assetLibrary.setTagStarred(tag.id, !tag.is_starred)}
                                title={tag.is_starred ? "Unstar" : "Star"}
                                class="shrink-0 {tag.is_starred
                                    ? 'text-amber-400'
                                    : 'text-neutral-600 hover:text-neutral-400'}"
                            >
                                {tag.is_starred ? "★" : "☆"}
                            </button>

                            <label class="shrink-0 cursor-pointer" title="Tag color">
                                <span
                                    class="block h-3.5 w-3.5 rounded-full border border-neutral-600"
                                    style="background-color: {swatch(tag)}"
                                ></span>
                                <input
                                    type="color"
                                    value={swatch(tag)}
                                    onchange={(e) =>
                                        assetLibrary.setTagColor(tag.id, e.currentTarget.value)}
                                    class="sr-only"
                                />
                            </label>

                            <button
                                type="button"
                                onclick={() => renameTag(tag)}
                                title="Rename"
                                class="flex-1 truncate text-left text-sm text-neutral-200"
                            >
                                {tag.name}
                            </button>

                            <span class="shrink-0 text-xs text-neutral-500">{tag.usage}</span>

                            <div
                                class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity
                                       group-hover:opacity-100"
                            >
                                <!-- Move to group -->
                                <select
                                    value={tag.group_id ?? ""}
                                    onchange={(e) =>
                                        assetLibrary.setTagGroup(tag.id, e.currentTarget.value || null)}
                                    title="Move to group"
                                    class="rounded border border-neutral-700 bg-neutral-900 px-1 py-0.5
                                           text-xs text-neutral-300"
                                >
                                    <option value="">No group</option>
                                    {#each groups as g (g.id)}
                                        <option value={g.id}>{g.name}</option>
                                    {/each}
                                </select>

                                <!-- Merge into another tag -->
                                <select
                                    value=""
                                    onchange={(e) => {
                                        const t = e.currentTarget;
                                        if (t.value) mergeInto(tag, t.value);
                                        t.value = "";
                                    }}
                                    title="Merge into…"
                                    class="rounded border border-neutral-700 bg-neutral-900 px-1 py-0.5
                                           text-xs text-neutral-300"
                                >
                                    <option value="">Merge…</option>
                                    {#each tags.filter((t) => t.id !== tag.id) as other (other.id)}
                                        <option value={other.id}>{other.name}</option>
                                    {/each}
                                </select>

                                <button
                                    type="button"
                                    onclick={() => deleteTag(tag)}
                                    title="Delete tag"
                                    class="px-1 text-neutral-500 hover:text-red-400">🗑</button
                                >
                            </div>
                        </div>
                    {/each}
                {/if}
            </div>
        </div>
    </div>
</div>
