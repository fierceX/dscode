<script setup lang="ts">
defineProps<{ path: string; content: string }>();
</script>

<template>
  <div class="file-path">{{ path }}</div>
  <pre>{{ content }}</pre>
</template>
