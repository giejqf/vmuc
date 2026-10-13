// SPDX-License-Identifier: Apache-2.0
// chokidar 4 (vite, nodemon): one line per event.
import { watch } from 'chokidar';

watch(process.argv[2], { ignoreInitial: true }).on('all', (ev, p) => {
    console.log(`${Date.now()} chokidar ${ev} ${p}`);
});
