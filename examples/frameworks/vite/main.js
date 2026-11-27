document.getElementById("buy").addEventListener("click", () => {
  const cart = undefined;
  console.log(cart.total); // TypeError, reported by the browser script the plugin adds
});
