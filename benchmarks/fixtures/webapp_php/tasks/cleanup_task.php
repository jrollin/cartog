<?php

namespace App\Tasks;

use App\Database\DatabaseConnection;
use App\Database\SessionQueries;

use function App\Utils\get_logger;

/**
 * Background task that purges expired sessions.
 */
class CleanupTask
{
    private SessionQueries $sessions;

    public function __construct(DatabaseConnection $db)
    {
        $this->sessions = new SessionQueries($db);
    }

    public function run(): int
    {
        get_logger('tasks.cleanup')->info('Running session cleanup');
        $purged = 0;
        foreach ($this->expiredSessionIds() as $id) {
            $this->sessions->expireSession($id);
            $purged++;
        }

        return $purged;
    }

    /**
     * @return list<int>
     */
    private function expiredSessionIds(): array
    {
        return [];
    }
}
