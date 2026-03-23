# Top-level non-async external calls.
# This matches Monty's plain external function style, where host-provided
# functions can be called directly without wrapping them in async code.

user = load_user_profile("user-123")
orders = list_recent_orders("user-123")
score = calculate_risk_score(user, orders)

print("user:", user)
print("orders:", orders)
print("score:", score)

{
    "user": user,
    "orders": orders,
    "score": score,
}
