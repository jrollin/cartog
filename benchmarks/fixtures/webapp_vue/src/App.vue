<template>
  <main>
    <LoginForm @loggedIn="onLoggedIn" />
    <UserBadge v-if="token" :token="token" />
  </main>
</template>

<script setup lang="ts">
import { ref } from "vue";
import LoginForm from "./components/LoginForm.vue";
import UserBadge from "./components/UserBadge.vue";

const token = ref("");

function onLoggedIn(value: string) {
  token.value = value;
}
</script>
