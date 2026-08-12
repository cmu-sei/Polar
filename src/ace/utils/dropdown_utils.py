from utils.neo4j_client import get_driver


def assemble_dropdown_selection():

    CYPHER_QUERY = """
        MATCH (t:JiraIssue_Agile_Team)
        RETURN t.value AS name
        ORDER BY name asc
    """

    def fetch_team_names(query):
        driver = get_driver()
        with driver.session() as session:
            result = session.run(query)
            team_list = [record["name"] for record in result]
        return team_list

    teams = fetch_team_names(CYPHER_QUERY)

    return teams

def assemble_pi_selection_initial(teams):
    # Ensure `team_names` is always a list
    team_names = [teams] if isinstance(teams, str) else teams

    CYPHER_QUERY = """
        MATCH (t:JiraIssue_Agile_Team)<-[:HAS_AGILE_TEAM]-(i:JiraIssue)
        MATCH (i)-[:HAS_PROGRAM_INCREMENT]->(pi:JiraIssue_Program_Increment)
        WHERE t.value IN $team_names AND pi.value IS NOT NULL
        WITH DISTINCT pi.value AS increment_value
        ORDER BY toInteger(REPLACE(increment_value, 'PI-', ""))
        RETURN increment_value
    """

    def fetch_increments(query, team_names):
        driver = get_driver()
        with driver.session() as session:
            result = session.run(query, parameters={"team_names": team_names})
            increments = [record["increment_value"] for record in result]
        return increments

    increments = fetch_increments(CYPHER_QUERY, team_names)

    increments = sorted(
    increments,
    key=lambda x: int(x.replace("PI-", "")),
    reverse = True
    )

    return increments

def assemble_pi_selection_training(teams, exclude_increment):
    # Ensure `team_names` is always a list
    team_names = [teams] if isinstance(teams, str) else teams
    CYPHER_QUERY = """
        MATCH (t:JiraIssue_Agile_Team)<-[:HAS_AGILE_TEAM]-(i:JiraIssue)
        MATCH (i)-[:HAS_PROGRAM_INCREMENT]->(pi:JiraIssue_Program_Increment)
        WHERE t.value IN $team_names
        AND pi.value IS NOT NULL
        AND toInteger(replace(trim(pi.value), 'PI-', ''))
            < toInteger(replace(trim($exclude_increment), 'PI-', ''))
        WITH DISTINCT pi.value AS increment_value
        ORDER BY toInteger(replace(trim(increment_value), 'PI-', ''))
        RETURN increment_value
    """

    def fetch_increments(query, team_names, exclude_increment):
        driver = get_driver()
        with driver.session() as session:
            result = session.run(query, parameters={
                "team_names": team_names,
                "exclude_increment": exclude_increment
            })
            increments = [record["increment_value"] for record in result]
        return increments

    increments = fetch_increments(CYPHER_QUERY, team_names, exclude_increment)

    increments = sorted(
    increments,
    key=lambda x: int(x.replace("PI-", "")),
    reverse = True
    )

    return increments
