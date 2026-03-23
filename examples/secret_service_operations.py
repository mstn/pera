agent = register_agent(
    "Nightjar",
    ["surveillance", "infiltration", "signals"],
    91,
    "Courier operations lead",
)

mission = create_mission(
    "Intercept the courier drop before sunrise",
    "high",
    "Trieste",
    ["surveillance", "signals"],
)

radio = add_inventory_item(
    "Encrypted Radio",
    "communications",
    2,
    '{"range_km": 12, "battery_hours": 8}',
)

assignment = assign_mission(mission.id, agent.id, [radio.id])

resolution = resolve_mission(
    mission.id,
    "success",
    "Courier route intercepted and package recovered.",
)

agents = list_agents(80, None)
missions = list_missions(None, "Trieste")
inventory = list_inventory("available", True)

print(agent)
print(assignment)
print(resolution)
print(agents)
print(missions)
print(inventory)
