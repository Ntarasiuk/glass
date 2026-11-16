// The host clipboard's image, fetched when needed (it can be several MB). The newest copy
// wins: each request has an id, and only the answer to the newest one is taken. A host too
// old to echo the id gets its answer taken for whichever request is waiting.

export function imageRequests({ ask, timeoutMs }) {
  let waiter;
  let lastId = 0;
  return {
    next() {
      if (waiter) waiter.reject(Object.assign(new Error('a newer copy replaced it'), { superseded: true }));
      return new Promise((resolve, reject) => {
        const w = (waiter = { id: ++lastId, resolve, reject });
        ask(w.id);
        setTimeout(() => {
          if (waiter !== w) return;
          waiter = undefined;
          reject(new Error('the image never came'));
        }, timeoutMs);
      });
    },
    received(image, id) {
      if (id !== undefined && id !== waiter?.id) return;
      waiter?.resolve(image);
      waiter = undefined;
    },
  };
}
