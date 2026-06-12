<script lang="ts">
  import LoginForm from "./components/LoginForm.svelte";
  import UserBadge from "./components/UserBadge.svelte";

  let token = "";

  function onLoggedIn(event: CustomEvent<string>) {
    token = event.detail;
  }
</script>

<main>
  <LoginForm on:loggedIn={onLoggedIn} />
  {#if token}
    <UserBadge {token} />
  {/if}
</main>
