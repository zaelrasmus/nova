<!--
  Segmented — pick one of a few. The selected segment is lifted (a raised chip),
  not filled with the accent: a row of these in the toolbar would otherwise be a
  row of bright blocks competing with the selection, which is the thing the
  accent is reserved for.
-->
<script lang="ts" generics="T extends string">
    import type { Component } from "svelte";

    interface Option {
        value: T;
        /** Text label. Also the tooltip when `icon` replaces it. */
        label: string;
        icon?: Component<{ class?: string }>;
        title?: string;
        disabled?: boolean;
    }

    interface Props {
        value: T;
        options: Option[];
        /** Accessible name for the group. */
        label: string;
        onchange: (value: T) => void;
    }
    let { value, options, label, onchange }: Props = $props();
</script>

<div
    role="radiogroup"
    aria-label={label}
    class="inline-flex w-fit shrink-0 gap-px rounded-lg border border-line bg-raised p-0.5"
>
    {#each options as o (o.value)}
        {@const Icon = o.icon}
        <button
            type="button"
            role="radio"
            aria-checked={value === o.value}
            aria-label={o.label}
            title={o.title ?? o.label}
            disabled={o.disabled}
            onclick={() => onchange(o.value)}
            class="grid h-6 min-w-7 place-items-center rounded-md text-xs font-medium transition-colors
                   focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-line
                   disabled:pointer-events-none disabled:opacity-35
                   {Icon ? 'px-1.5' : 'px-2.5'}
                   {value === o.value
                ? 'bg-raised-2 text-neutral-100 shadow-[0_1px_2px_rgb(0_0_0/0.45),inset_0_0_0_1px_var(--nv-line)]'
                : 'text-neutral-500 hover:text-neutral-200'}"
        >
            {#if Icon}
                <Icon class="h-3.5 w-3.5" />
            {:else}
                {o.label}
            {/if}
        </button>
    {/each}
</div>
