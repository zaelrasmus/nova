<!--
  Settings › Appearance. Everything here is paint, applied live by
  $lib/appearance.ts the moment a preference changes — no restart, no reload.

  Sliders write `settings.preferences` directly while dragging (live preview)
  and persist once on release, the same split DisplaySection uses.
-->
<script lang="ts">
    import { settings, type AppPreferences } from "../../routes/settings.svelte";
    import {
        ACCENTS,
        APPEARANCE_DEFAULTS,
        APPEARANCE_LIMITS,
        clampAppearance,
        type AccentName,
    } from "$lib/appearance";
    import { Label } from "$components/ui/label";
    import Segmented from "$components/controls/Segmented.svelte";
    import Switch from "$components/controls/Switch.svelte";
    import Range from "$components/controls/Range.svelte";

    const prefs = $derived(settings.preferences);

    type SliderKey = keyof typeof APPEARANCE_LIMITS;
    const SLIDERS: { key: SliderKey; label: string; hint: string }[] = [
        { key: "gridMarginX", label: "Grid margin — left and right", hint: "Space between the grid and the side panels." },
        { key: "gridMarginY", label: "Grid margin — top and bottom", hint: "Space above the first row and below the last." },
        { key: "gridGap", label: "Tile gap", hint: "Space between thumbnails." },
        { key: "tileRadius", label: "Corner radius", hint: "Roundness of thumbnails and folder tiles." },
    ];

    function live<K extends keyof AppPreferences>(key: K, value: AppPreferences[K]) {
        settings.preferences[key] = value;
    }

    const isDefault = $derived(
        (Object.keys(APPEARANCE_DEFAULTS) as (keyof typeof APPEARANCE_DEFAULTS)[]).every(
            (k) => prefs[k] === APPEARANCE_DEFAULTS[k],
        ),
    );
</script>

{#snippet row(label: string, hint: string)}
    <div class="flex flex-col gap-0.5">
        <Label class="text-sm text-neutral-200">{label}</Label>
        <p class="text-xs text-neutral-500">{hint}</p>
    </div>
{/snippet}

<div class="flex flex-col gap-6">
    <!-- Accent -->
    <div class="flex items-center justify-between gap-8">
        {@render row("Accent colour", "Marks what's selected, switched on, and where you are.")}
        <div role="radiogroup" aria-label="Accent colour" class="flex shrink-0 gap-2">
            {#each Object.entries(ACCENTS) as [name, accent] (name)}
                <button
                    type="button"
                    role="radio"
                    aria-checked={prefs.accent === name}
                    aria-label={accent.label}
                    title={accent.label}
                    onclick={() => settings.set("accent", name as AccentName)}
                    style:background-color={accent.value}
                    class="h-5 w-5 rounded-full ring-1 ring-inset ring-white/15 transition-shadow
                           focus-visible:outline-none
                           {prefs.accent === name
                        ? 'shadow-[0_0_0_2px_var(--nv-canvas),0_0_0_3.5px_white]'
                        : 'hover:shadow-[0_0_0_2px_var(--nv-canvas),0_0_0_3.5px_rgb(255_255_255/0.3)]'}"
                ></button>
            {/each}
        </div>
    </div>

    <!-- Pane tone -->
    <div class="flex items-center justify-between gap-8">
        {@render row("Panels", "Layered sets the sidebar and inspector a shade above the grid.")}
        <Segmented
            label="Panels"
            value={prefs.paneTone}
            options={[
                { value: "layered", label: "Layered" },
                { value: "flat", label: "Flat" },
            ]}
            onchange={(v) => settings.set("paneTone", v)}
        />
    </div>

    <!-- Spacing sliders -->
    {#each SLIDERS as s (s.key)}
        {@const limits = APPEARANCE_LIMITS[s.key]}
        <!-- Show what is applied, not what is stored: a hand-edited
             settings.json can hold a value the clamp overrides. -->
        {@const applied = clampAppearance(s.key, prefs[s.key])}
        <div class="flex flex-col gap-2">
            <div class="flex items-center justify-between gap-8">
                {@render row(s.label, s.hint)}
                <span class="w-12 shrink-0 text-right text-sm tabular-nums text-neutral-300">
                    {applied}px
                </span>
            </div>
            <Range
                label={s.label}
                value={applied}
                min={limits.min}
                max={limits.max}
                oninput={(v) => live(s.key, v)}
                oncommit={(v) => settings.set(s.key, v)}
                class="w-full"
            />
        </div>
    {/each}

    <!-- Tile info -->
    <div class="flex items-center justify-between gap-8">
        {@render row("Name on thumbnails", "Show each file's name and size over its thumbnail.")}
        <Segmented
            label="Name on thumbnails"
            value={prefs.tileInfo}
            options={[
                { value: "off", label: "Off" },
                { value: "hover", label: "On hover" },
                { value: "always", label: "Always" },
            ]}
            onchange={(v) => settings.set("tileInfo", v)}
        />
    </div>

    <!-- Select circle -->
    <div class="flex items-center justify-between gap-8">
        {@render row(
            "Selection circle",
            "A circle in the corner of a hovered thumbnail. Click it to add or remove that item from the selection.",
        )}
        <Switch
            label="Selection circle"
            checked={prefs.selectCircle}
            onchange={(v) => settings.set("selectCircle", v)}
        />
    </div>

    <!-- Density -->
    <div class="flex items-center justify-between gap-8">
        {@render row("Density", "Compact fits more rows in the sidebar and lowers the header.")}
        <Segmented
            label="Density"
            value={prefs.density}
            options={[
                { value: "comfortable", label: "Comfortable" },
                { value: "compact", label: "Compact" },
            ]}
            onchange={(v) => settings.set("density", v)}
        />
    </div>

    <div class="flex items-center justify-between gap-8 border-t border-line pt-5">
        {@render row("Reset appearance", "Return every setting on this page to its default.")}
        <button
            type="button"
            disabled={isDefault}
            onclick={() => settings.update({ ...APPEARANCE_DEFAULTS })}
            class="shrink-0 rounded-md bg-raised-2 px-3 py-1.5 text-xs font-medium text-neutral-100
                   ring-1 ring-inset ring-line-strong transition-colors hover:bg-neutral-700
                   disabled:cursor-not-allowed disabled:opacity-40"
        >
            Reset
        </button>
    </div>
</div>
