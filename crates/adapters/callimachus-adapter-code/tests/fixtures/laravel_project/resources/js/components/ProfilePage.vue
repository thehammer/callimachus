<template>
  <div class="profile-page">
    <h1>{{ name }}</h1>
    <input v-model="name" placeholder="Enter name" />
    <button @click="save">Save</button>
  </div>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue';

const name = ref('');
const email = ref('');

const displayName = computed(() => name.value || 'Anonymous');

function save(): void {
  // Persist the profile
  console.log('Saving', name.value, email.value);
}

function reset(): void {
  // Clear all profile fields back to their initial state.
  name.value = '';
  email.value = '';
  // Notify parent components that the form was reset.
  console.log('Profile form reset to defaults');
}
</script>

<style scoped>
.profile-page {
  max-width: 600px;
}
</style>
