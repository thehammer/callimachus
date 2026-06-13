<template>
  <div class="user-card">
    <h2>{{ user.name }}</h2>
    <p>{{ user.email }}</p>
    <button @click="selectUser">Select</button>
  </div>
</template>

<script>
export default {
  name: 'UserCard',
  props: {
    user: {
      type: Object,
      required: true,
    },
  },
  methods: {
    selectUser() {
      this.$emit('select', this.user);
    },
  },
};
</script>

<style scoped>
.user-card {
  border: 1px solid #ccc;
  padding: 1rem;
}
</style>
