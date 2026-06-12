<template>
  <span class="badge">{{ label }}</span>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { AuthService } from "../lib/auth";

const props = defineProps<{ token: string }>();
const auth = new AuthService();

const label = computed(() => describe(props.token));

function describe(token: string): string {
  const claims = auth.currentUser(token);
  return `user:${claims.userId}`;
}
</script>
