agent = register_agent(
    "Nightjar",
    ["surveillance", "infiltration", "signals"],
    91,
    "Courier operations lead",
)

mission = create_mission(
    "Intercept the courier drop before sunrise",
    "high",
    "Trieste Harbor",
    ["surveillance", "signals"],
)

radio = add_inventory_item(
    "Encrypted Radio",
    "communications",
    2,
    '{"range_km": 12, "battery_hours": 8}',
)

forecast = get_forecast(mission.region, "tomorrow")
advice = assess_travel_window(
    forecast.condition,
    forecast.wind_level,
    forecast.visibility_level,
)
weather_summary = build_weather_summary(mission.region, forecast)

assignment = None
resolution = None

if advice.advisable:
    assignment = assign_mission(mission.id, agent.id, [radio.id])
    resolution = resolve_mission(
        mission.id,
        "success",
        f"Mission completed. Weather: {weather_summary}",
    )
else:
    resolution = resolve_mission(
        mission.id,
        "aborted",
        "Mission postponed due to weather. " + "; ".join(advice.reasons),
    )

print(forecast)
print(advice)
print(weather_summary)
print(assignment)
print(resolution)
