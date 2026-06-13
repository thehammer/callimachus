<?php

namespace App\Services;

/**
 * Manages user accounts.
 */
class UserService
{
    private array $users = [];

    /**
     * Find a user by ID.
     *
     * @param int $id
     * @return array|null
     */
    public function findById(int $id): ?array
    {
        return $this->users[$id] ?? null;
    }

    /**
     * Create a new user.
     *
     * @param string $name
     * @param string $email
     * @return array
     */
    public function create(string $name, string $email): array
    {
        $user = ['id' => count($this->users) + 1, 'name' => $name, 'email' => $email];
        $this->users[$user['id']] = $user;
        return $user;
    }
}
