<!--
  Switch — the app's one on/off control. Settings, the Display popover and the
  inspector all use this, so "on" is the accent everywhere and nowhere else.
-->
<script lang="ts">
    interface Props {
        checked: boolean;
        /** Accessible name. Required: a switch is usually next to, not inside, its label. */
        label: string;
        disabled?: boolean;
        onchange: (checked: boolean) => void;
    }
    let { checked, label, disabled = false, onchange }: Props = $props();
</script>

<button
    type="button"
    role="switch"
    aria-checked={checked}
    aria-label={label}
    {disabled}
    onclick={() => onchange(!checked)}
    class="relative inline-flex h-[18px] w-8 shrink-0 items-center rounded-full transition-colors
           focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-line
           disabled:opacity-40
           {checked ? 'bg-brand' : 'bg-raised-2 ring-1 ring-inset ring-line-strong'}"
>
    <span
        class="inline-block h-3.5 w-3.5 rounded-full shadow-sm transition-transform
               {checked ? 'translate-x-4 bg-white' : 'translate-x-0.5 bg-neutral-300'}"
    ></span>
</button>
