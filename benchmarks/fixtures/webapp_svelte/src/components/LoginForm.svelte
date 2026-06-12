<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { AuthService } from "../lib/auth";

  let name = "";
  const auth = new AuthService();
  const dispatch = createEventDispatcher<{ loggedIn: string }>();

  function onSubmit() {
    const token = auth.login(name);
    dispatch("loggedIn", token);
  }
</script>

<form on:submit|preventDefault={onSubmit}>
  <input bind:value={name} />
  <button type="submit">Log in</button>
</form>
