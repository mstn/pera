from wit_world import exports
from wit_world.imports import weather_brief_types


def _condition_for_location(location: str) -> weather_brief_types.WeatherCondition:
    key = (location or "").strip().lower()
    if any(token in key for token in ("london", "dublin", "bergen", "trieste")):
        return weather_brief_types.WeatherCondition.RAIN
    if any(token in key for token in ("reykjavik", "oslo", "helsinki")):
        return weather_brief_types.WeatherCondition.SNOW
    if any(token in key for token in ("venice", "harbor", "port")):
        return weather_brief_types.WeatherCondition.FOG
    if any(token in key for token in ("athens", "sicily", "valencia")):
        return weather_brief_types.WeatherCondition.CLEAR
    return weather_brief_types.WeatherCondition.CLOUDY


def _forecast_shape(location: str, day: str) -> weather_brief_types.WeatherReport:
    condition = _condition_for_location(location)

    if condition == weather_brief_types.WeatherCondition.CLEAR:
        return weather_brief_types.WeatherReport(
            location=location,
            day=day,
            condition=condition,
            temperature_c=24,
            wind_level=2,
            visibility_level=9,
            advisories=["Stable conditions expected."],
        )
    if condition == weather_brief_types.WeatherCondition.RAIN:
        return weather_brief_types.WeatherReport(
            location=location,
            day=day,
            condition=condition,
            temperature_c=12,
            wind_level=5,
            visibility_level=5,
            advisories=["Carry waterproof gear.", "Allow extra travel time."],
        )
    if condition == weather_brief_types.WeatherCondition.FOG:
        return weather_brief_types.WeatherReport(
            location=location,
            day=day,
            condition=condition,
            temperature_c=10,
            wind_level=2,
            visibility_level=3,
            advisories=["Limited line of sight.", "Prefer close-range coordination."],
        )
    if condition == weather_brief_types.WeatherCondition.SNOW:
        return weather_brief_types.WeatherReport(
            location=location,
            day=day,
            condition=condition,
            temperature_c=-2,
            wind_level=4,
            visibility_level=4,
            advisories=["Ice risk on roads.", "Cold-weather gear recommended."],
        )

    return weather_brief_types.WeatherReport(
        location=location,
        day=day,
        condition=condition,
        temperature_c=17,
        wind_level=3,
        visibility_level=7,
        advisories=["Conditions are serviceable."],
    )


class WeatherBriefExports(exports.WeatherBriefExports):
    def get_forecast(self, location, day):
        return _forecast_shape(location, day)

    def assess_travel_window(self, condition, wind_level, visibility_level):
        condition_name = getattr(condition, "name", str(condition)).lower()
        reasons: list[str] = []
        advisable = True

        if condition_name in ("storm", "snow"):
            advisable = False
            reasons.append("Severe conditions increase operational risk.")
        if condition_name == "fog":
            reasons.append("Low visibility may delay movement.")
        if int(wind_level) >= 7:
            advisable = False
            reasons.append("Wind is too strong for safe travel.")
        if int(visibility_level) <= 3:
            advisable = False
            reasons.append("Visibility is too low for reliable field work.")
        if not reasons:
            reasons.append("Weather window is acceptable.")

        return weather_brief_types.TravelAdvice(advisable=advisable, reasons=reasons)

    def build_weather_summary(self, location, forecast):
        condition_name = getattr(forecast.condition, "name", str(forecast.condition)).lower()
        advisories = ", ".join(forecast.advisories or [])
        return (
            f"{location} on {forecast.day}: {condition_name}, "
            f"{forecast.temperature_c}C, wind {forecast.wind_level}/10, "
            f"visibility {forecast.visibility_level}/10. "
            f"Notes: {advisories}"
        )
