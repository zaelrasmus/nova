<!--
  Range — the styled slider (`.nv-range` in layout.css). The filled part of the
  track is a gradient sized by `--fill`, which only this wrapper knows how to
  compute, so every slider goes through here rather than styling its own input.

  `oninput` fires per tick (live preview), `oncommit` on release (persist):
  a drag is then one disk write, not two hundred.
-->
<script lang="ts">
    interface Props {
        value: number;
        min: number;
        max: number;
        step?: number;
        label: string;
        disabled?: boolean;
        class?: string;
        oninput?: (value: number) => void;
        oncommit?: (value: number) => void;
    }
    let {
        value = $bindable(),
        min,
        max,
        step = 1,
        label,
        disabled = false,
        class: className = "",
        oninput,
        oncommit,
    }: Props = $props();

    const fill = $derived(`${max > min ? ((value - min) / (max - min)) * 100 : 0}%`);
</script>

<input
    type="range"
    {min}
    {max}
    {step}
    {disabled}
    aria-label={label}
    bind:value
    style:--fill={fill}
    oninput={() => oninput?.(value)}
    onchange={() => oncommit?.(value)}
    class="nv-range {className}"
/>
