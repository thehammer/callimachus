<template>
  <div class="user-card">
    <h2>{{ user.name }}</h2>
    <p>{{ user.email }}</p>
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue';

export default defineComponent({
  name: 'UserCard',
  props: {
    user: {
      type: Object,
      required: true,
    },
  },
});

function formatEmail(email: string): string {
  return email.toLowerCase().trim();
}
</script>

<style scoped>
.user-card {
  padding: 1rem;
}
</style>
