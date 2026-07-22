<script lang="ts">
    import { settings } from "../../routes/settings.svelte";
    import * as Select from "$components/ui/select";
    import { Label } from "$components/ui/label";

    const themeOptions = [
        { value: "system", label: "System" },
        { value: "light", label: "Light" },
        { value: "dark", label: "Dark" },
    ] as const;

    const fontSizeOptions = [
        { value: "sm", label: "Small" },
        { value: "md", label: "Medium" },
        { value: "lg", label: "Large" },
    ] as const;
</script>

<div class="flex flex-col gap-6">
    <!-- ── Row pattern ────────────────────────────────────────────────────────
       Every setting follows this structure:
       - Left: label + optional description
       - Right: the control
       Add new settings by copying a row block below.
  ─────────────────────────────────────────────────────────────────────────-->

    <div class="flex items-center justify-between gap-8">
        <div class="flex flex-col gap-0.5">
            <Label class="text-sm text-neutral-200">Theme</Label>
            <p class="text-xs text-neutral-500">Choose your preferred color scheme.</p>
        </div>
        <!-- <Select.Root
            selected={{ value: settings.preferences.theme, label: settings.preferences.theme }}
            onSelectedChange={(v) =>
                v && settings.set("theme", v.value as typeof settings.preferences.theme)}
        >
            <Select.Trigger class="w-32 bg-neutral-900 border-neutral-700 text-neutral-200">
                <Select.Value />
            </Select.Trigger>
            <Select.Content class="bg-neutral-900 border-neutral-700">
                {#each themeOptions as opt}
                    <Select.Item
                        value={opt.value}
                        class="text-neutral-200 focus:bg-neutral-800"
                    >
                        {opt.label}
                    </Select.Item>
                {/each}
            </Select.Content>
        </Select.Root> -->
    </div>

    <div class="h-px bg-neutral-800" aria-hidden="true"></div>

    <div class="flex items-center justify-between gap-8">
        <div class="flex flex-col gap-0.5">
            <Label class="text-sm text-neutral-200">Font Size</Label>
            <p class="text-xs text-neutral-500">Controls text size across the interface.</p>
        </div>
        <!-- <Select.Root
            selected={{
                value: settings.preferences.fontSize,
                label: settings.preferences.fontSize,
            }}
            onSelectedChange={(v) =>
                v && settings.set("fontSize", v.value as typeof settings.preferences.fontSize)}
        >
            <Select.Trigger class="w-32 bg-neutral-900 border-neutral-700 text-neutral-200">
                <Select.Value />
            </Select.Trigger>
            <Select.Content class="bg-neutral-900 border-neutral-700">
                {#each fontSizeOptions as opt}
                    <Select.Item
                        value={opt.value}
                        class="text-neutral-200 focus:bg-neutral-800"
                    >
                        {opt.label}
                    </Select.Item>
                {/each}
            </Select.Content>
        </Select.Root> -->
    </div>
</div>
