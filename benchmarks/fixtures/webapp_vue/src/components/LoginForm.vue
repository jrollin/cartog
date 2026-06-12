<template>
  <form @submit.prevent="onSubmit">
    <input v-model="name" />
    <button type="submit">Log in</button>
  </form>
</template>

<script setup lang="ts">
import { ref } from "vue";
import { AuthService } from "../lib/auth";

const name = ref("");
const auth = new AuthService();
const emit = defineEmits<{ (e: "loggedIn", token: string): void }>();

function onSubmit() {
  const token = auth.login(name.value);
  emit("loggedIn", token);
}
</script>
