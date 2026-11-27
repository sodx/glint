export function GET() {
  const order = { id: 7 };
  return Response.json({ avatar: order.customer.avatar }); // TypeError
}
