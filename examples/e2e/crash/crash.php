<?php
// No Glint code: the uncaught exception is reported by the global install.
function pay(array $cart): float { return $cart["total"] ?? throw new DomainException("cart has no total"); }
pay([]);
