<script setup lang="ts">
defineProps<{ content: string }>();
</script>

<template><pre class="t-search">{{ content }}</pre></template>

<style scoped>
.t-search { margin: 0; white-space: pre-wrap; font-family: var(--mono); font-size: 12px; color: var(--text-soft); }
</style>
