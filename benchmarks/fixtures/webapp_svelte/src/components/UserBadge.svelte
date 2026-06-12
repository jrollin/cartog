<script lang="ts">
  import { AuthService } from "../lib/auth";

  export let token: string;
  const auth = new AuthService();

  function describe(value: string): string {
    const claims = auth.currentUser(value);
    return `user:${claims.userId}`;
  }

  $: label = describe(token);
</script>

<span class="badge">{label}</span>
